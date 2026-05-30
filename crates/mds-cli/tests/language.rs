mod common;
use common::fixture;
use std::collections::HashMap;

#[test]
fn simple_variable_interpolation() {
    let result = mds::compile(fixture("simple.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("You have 3 items."));
}

#[test]
fn conditional_truthy() {
    let result = mds::compile(fixture("conditional.mds"), None).unwrap();
    assert!(result.contains("Thanks for being premium!"));
    assert!(!result.contains("Upgrade for premium features."));
}

#[test]
fn conditional_falsy() {
    let result = mds::compile(fixture("conditional_false.mds"), None).unwrap();
    assert!(!result.contains("Thanks for being premium!"));
    assert!(result.contains("Upgrade for premium features."));
}

#[test]
fn loop_over_array() {
    let result = mds::compile(fixture("loop.mds"), None).unwrap();
    assert!(result.contains("- apple"));
    assert!(result.contains("- banana"));
    assert!(result.contains("- cherry"));
}

#[test]
fn function_definition_and_call() {
    let result = mds::compile(fixture("function.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("Hello Bob!"));
}

#[test]
fn escaped_braces() {
    let result = mds::compile(fixture("escaped.mds"), None).unwrap();
    assert!(result.contains("{name}"));
}

#[test]
fn code_block_passthrough() {
    let result = mds::compile(fixture("code_block.mds"), None).unwrap();
    // Inside code block: no interpolation should occur
    assert!(result.contains("{not_a_var}"));
    assert!(result.contains("{world}"));
    // Outside code block: interpolation should work
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("Goodbye Alice!"));
}

#[test]
fn runtime_vars_override() {
    let mut vars = HashMap::new();
    vars.insert(
        "name".to_string(),
        mds::Value::String("Override".to_string()),
    );
    let result = mds::compile(fixture("simple.mds"), Some(vars)).unwrap();
    assert!(result.contains("Hello Override!"));
}

#[test]
fn complete_example_welcome() {
    let result = mds::compile(fixture("welcome.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("- apple"));
    assert!(result.contains("- banana"));
    assert!(result.contains("Thanks for being premium!"));
    assert!(!result.contains("Upgrade for premium features."));
    assert!(result.contains("Thank you for using our service."));
}

#[test]
fn unicode_content() {
    let result = mds::compile(fixture("unicode.mds"), None).unwrap();
    assert!(result.contains("Greetings Rene!"));
    assert!(result.contains("Hello"));
    // Code block content should not be interpolated
    assert!(result.contains("{not_interpolated}"));
    assert!(result.contains("Farewell Rene!"));
}

#[test]
fn compile_str_simple() {
    let source = "---\nname: World\n---\nHello {name}!\n";
    let result = mds::compile_str_with(source, None, None).unwrap();
    assert!(result.contains("Hello World!"));
}

#[test]
fn compile_str_no_frontmatter() {
    let result = mds::compile_str_with("Just plain text.", None, None).unwrap();
    assert!(result.contains("Just plain text."));
}

#[test]
fn nested_loops() {
    let result = mds::compile(fixture("nested_loops.mds"), None).unwrap();
    assert!(result.contains("row1-col1"), "nested loops: row1-col1");
    assert!(result.contains("row1-col2"), "nested loops: row1-col2");
    assert!(result.contains("row2-col1"), "nested loops: row2-col1");
    assert!(result.contains("row2-col2"), "nested loops: row2-col2");
}

#[test]
fn function_called_in_loop() {
    let result = mds::compile(fixture("fn_in_loop.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"), "fn in loop: Alice");
    assert!(result.contains("Hello Bob!"), "fn in loop: Bob");
    assert!(result.contains("Hello Charlie!"), "fn in loop: Charlie");
}

#[test]
fn loop_var_shadows_outer() {
    let result = mds::compile(fixture("loop_var_shadow.mds"), None).unwrap();
    // Before loop, outer value
    assert!(
        result.contains("Before: outer_value"),
        "before loop: outer_value"
    );
    // During loop, inner values
    assert!(result.contains("- inner1"), "loop iteration: inner1");
    assert!(result.contains("- inner2"), "loop iteration: inner2");
    // After loop, restored outer value
    assert!(
        result.contains("After: outer_value"),
        "after loop: outer_value restored"
    );
}

#[test]
fn function_param_shadows_outer() {
    let result = mds::compile(fixture("fn_param_shadow.mds"), None).unwrap();
    assert!(
        result.contains("Before: outer"),
        "before fn call: outer name"
    );
    assert!(
        result.contains("Hello inner!"),
        "inside fn: param shadows outer"
    );
    assert!(
        result.contains("After: outer"),
        "after fn call: outer name restored"
    );
}

#[test]
fn nested_function_calls_in_interpolation() {
    let result = mds::compile(fixture("nested_fn_calls.mds"), None).unwrap();
    // outer(inner("arg")) => outer("arg!") => "[arg!]"
    assert!(
        result.contains("[arg!]"),
        "nested fn calls should produce '[arg!]', got: {result}"
    );
}

#[test]
fn multiple_escaped_braces() {
    let result = mds::compile(fixture("multiple_escaped_braces.mds"), None).unwrap();
    // \{a\} → literal '{a}' and \{b\} → literal '{b}'
    // Per spec: both \{ and \} are escape sequences, producing literal { and }
    assert!(
        result.contains("{a") && result.contains("{b"),
        "escaped braces should produce literal '{{', got: {result}"
    );
}

#[test]
fn if_falsy_zero() {
    let result = mds::compile(fixture("if_falsy_zero.mds"), None).unwrap();
    assert!(
        result.contains("falsy"),
        "zero should be falsy, got: {result}"
    );
    assert!(
        !result.contains("truthy"),
        "zero should not be truthy, got: {result}"
    );
}

#[test]
fn if_falsy_null() {
    let result = mds::compile(fixture("if_falsy_null.mds"), None).unwrap();
    assert!(
        result.contains("falsy"),
        "null should be falsy, got: {result}"
    );
}

#[test]
fn if_falsy_empty_string() {
    let result = mds::compile(fixture("if_falsy_empty_string.mds"), None).unwrap();
    assert!(
        result.contains("falsy"),
        "empty string should be falsy, got: {result}"
    );
}

#[test]
fn if_falsy_empty_array() {
    let result = mds::compile(fixture("if_falsy_empty_array.mds"), None).unwrap();
    assert!(
        result.contains("falsy"),
        "empty array should be falsy, got: {result}"
    );
}

#[test]
fn if_falsy_boolean_false() {
    // `false` is explicitly listed as a falsy value in Spec 4.3.
    let result = mds::compile(fixture("if_falsy_false.mds"), None).unwrap();
    assert!(
        result.contains("falsy"),
        "boolean false should be falsy, got: {result}"
    );
    assert!(
        !result.contains("truthy"),
        "boolean false should not be truthy, got: {result}"
    );
}

#[test]
fn mutual_recursion_detected() {
    let result = mds::compile(fixture("mutual_recursion.mds"), None);
    assert!(result.is_err(), "mutual recursion should be detected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("recursion"),
        "expected recursion error, got: {err}"
    );
}

#[test]
fn crlf_line_endings() {
    // A fixture with Windows (CRLF) line endings must compile without error
    // and produce the same output as its LF counterpart.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("crlf.mds");
    // Write the file with \r\n line endings (Windows style).
    std::fs::write(&path, b"---\r\nname: Alice\r\n---\r\nHello {name}!\r\n").unwrap();

    let result = mds::compile(&path, None).unwrap();
    assert!(
        result.contains("Hello Alice!"),
        "CRLF line endings should compile correctly, got: {result}"
    );
}

#[test]
fn multi_param_function() {
    // @define welcome(name, role): with two params, per spec section 4.5
    let result = mds::compile(fixture("multi_param.mds"), None).unwrap();
    assert!(
        result.contains("Hello Alice! You are logged in as admin."),
        "two-param function call with string literals should render correctly, got: {result}"
    );
    assert!(
        result.contains("Hello Bob! You are logged in as editor."),
        "second two-param call should render correctly, got: {result}"
    );
}

#[test]
fn single_quote_string_literal_in_function_args() {
    // {greet('Alice')} — single-quoted string literals in function arguments.
    // The parser already supports this; this test locks in the behaviour.
    let result = mds::compile(fixture("single_quote_args.mds"), None).unwrap();
    assert!(
        result.contains("Hello Alice!"),
        "single-quoted arg should produce same output as double-quoted, got: {result}"
    );
    assert!(
        result.contains("Hello Bob!"),
        "second single-quoted arg call should render correctly, got: {result}"
    );
}

#[test]
fn zero_parameter_function() {
    // @define separator(): produces a fixed separator string with no params
    let source = "@define separator():\n---\n@end\n{separator()}\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("---"),
        "zero-parameter function should produce its body, got: {result}"
    );
}

#[test]
fn empty_function_body() {
    // @define empty(): @end — calling it should succeed and produce empty string
    let source = "@define empty():\n@end\nBefore{empty()}After\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("BeforeAfter"),
        "empty function body should produce empty string, got: {result}"
    );
}

#[test]
fn deeply_nested_conditionals() {
    let source = "---\na: true\nb: true\nc: true\n---\n\
        @if a:\n\
        @if b:\n\
        @if c:\n\
        deep\n\
        @end\n\
        @end\n\
        @end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("deep"),
        "3-level nested @if should reach innermost body, got: {result}"
    );
}

#[test]
fn function_returning_inner_call() {
    let source = "\
        @define inner():\n\
        inner-result\n\
        @end\n\
        @define outer():\n\
        {inner()}\n\
        @end\n\
        {outer()}\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("inner-result"),
        "outer() should return result of inner(), got: {result}"
    );
}

#[test]
fn loop_single_element_array() {
    let source = "---\nitems: [only]\n---\n@for item in items:\n- {item}\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("- only"),
        "single-element array loop should produce one item, got: {result}"
    );
}

#[test]
fn escaped_brace_inside_function_body() {
    // \{ in MDS source renders as a literal {, so \{literal} produces {literal}
    let source = "@define show():\n\\{literal}\n@end\n{show()}\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("{literal}"),
        "escaped brace inside function body should render as literal brace, got: {result}"
    );
}

#[test]
fn variable_interpolation_in_function_argument() {
    // {greet(name)} where name is a frontmatter variable
    let source = "---\nname: Alice\n---\n@define greet(who):\nHello {who}!\n@end\n{greet(name)}\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("Hello Alice!"),
        "variable passed as function argument should be resolved, got: {result}"
    );
}

#[test]
fn escaped_braces_in_function_body() {
    // Per spec: both \{ and \} are escape sequences producing literal braces.
    // So \{curly braces\} → "{curly braces}" in output.
    let result = mds::compile(fixture("escaped_brace_in_fn.mds"), None).unwrap();
    assert!(
        result.contains("{curly braces"),
        "escaped brace in function body should produce literal brace, got: {result}"
    );
    assert!(
        result.contains("Alice"),
        "interpolation inside function body should still work, got: {result}"
    );
}

#[test]
fn escaped_braces_in_blocks() {
    // Per spec: both \{ and \} are escape sequences producing literal braces.
    // So \{variable\} => "{variable}" and \{item\} => "{item}".
    let result = mds::compile(fixture("escaped_brace_in_blocks.mds"), None).unwrap();
    assert!(
        result.contains("{variable"),
        "escaped brace in @if body should produce literal brace, got: {result}"
    );
    assert!(
        result.contains("{item") && result.contains("= a"),
        "escaped brace in @for body should produce literal brace for 'a', got: {result}"
    );
    assert!(
        result.contains("{item") && result.contains("= b"),
        "escaped brace in @for body should produce literal brace for 'b', got: {result}"
    );
}

#[test]
fn escaped_close_brace_produces_literal_brace() {
    // `\}` should produce a literal `}` in output, symmetric with `\{` → `{`.
    let result = mds::compile_str("Use \\} to close.").unwrap();
    assert!(
        result.contains('}'),
        "\\}} should produce a literal `}}`, got: {result}"
    );
    assert!(
        !result.contains("\\}"),
        "backslash should be stripped before `}}`, got: {result}"
    );
}

#[test]
fn escaped_open_and_close_brace_together() {
    // `\{not interpolated\}` should produce `{not interpolated}` in output.
    let result = mds::compile_str("\\{not interpolated\\}").unwrap();
    assert!(
        result.contains("{not interpolated}"),
        "expected `{{not interpolated}}` in output, got: {result}"
    );
}

#[test]
fn escaped_close_brace_in_function_body() {
    // `\}` inside a @define body should also produce a literal `}`.
    let source = "@define show():\nresult\\}\n@end\n{show()}\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("result}"),
        "escaped `}}` inside function body should render as literal `}}`, got: {result}"
    );
}

#[test]
fn loop_var_not_visible_after_loop() {
    // The loop variable is scoped to the @for...@end block.
    // After the loop, attempting to use it should fail.
    let source = "---\nitems: [a, b]\n---\n@for item in items:\n- {item}\n@end\n{item}\n";
    let result = mds::compile_str(source);
    assert!(
        result.is_err(),
        "loop variable should not be visible after @end"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("item") || err.contains("undefined"),
        "error should mention undefined 'item' after loop, got: {err}"
    );
}

#[test]
fn function_param_not_visible_outside_function() {
    // Function parameters are scoped to the function body.
    // After the call, the param name is not in scope.
    let source = "@define greet(name):\nHello {name}!\n@end\n{greet(\"Alice\")}\n{name}\n";
    let result = mds::compile_str(source);
    assert!(
        result.is_err(),
        "function param should not be visible outside function body"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("name") || err.contains("undefined"),
        "error should mention undefined 'name' outside function, got: {err}"
    );
}

#[test]
fn compile_str_empty_frontmatter() {
    // "---\n---\n" is valid: empty frontmatter with no variables.
    let source = "---\n---\nHello World!\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("Hello World!"),
        "empty frontmatter should compile cleanly, got: {result}"
    );
}

#[test]
fn compile_str_truly_no_frontmatter() {
    // Source with no --- fences at all is valid per spec (frontmatter is optional).
    let source = "@define greet(name):\nHi {name}!\n@end\n{greet(\"World\")}\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("Hi World!"),
        "source with no frontmatter and @define should compile, got: {result}"
    );
}

#[test]
fn for_null_iterable_rejected_at_check_time() {
    // Per spec: iterating over a non-array is a compilation error.
    // `null` must be rejected at validation time so `mds check` and `mds build`
    // both fail consistently — the validator must not accept Value::Null.
    let result = mds::check(fixture("for_null_iterable.mds"), None);
    assert!(
        result.is_err(),
        "@for over a null iterable must fail at check time (validator)"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("array") || err.contains("type error") || err.contains("null"),
        "error should mention array/type mismatch, got: {err}"
    );
}

#[test]
fn for_null_iterable_rejected_at_build_time() {
    // Same fixture — build must also fail (was already failing; test documents
    // that check and build agree after removing Null from the validator allowlist).
    let result = mds::compile(fixture("for_null_iterable.mds"), None);
    assert!(
        result.is_err(),
        "@for over a null iterable must fail at build time (evaluator)"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("array") || err.contains("type error") || err.contains("null"),
        "error should mention array/type mismatch, got: {err}"
    );
}

#[test]
fn empty_array_loop() {
    // Iterating over an empty array should produce no output for the loop body
    let mut vars = HashMap::new();
    vars.insert("items".to_string(), mds::Value::Array(vec![]));
    let result = mds::compile(fixture("loop.mds"), Some(vars)).unwrap();
    assert!(!result.contains("- apple"));
    assert!(!result.contains("- banana"));
}

#[test]
fn md_file_with_type_mds_compiles() {
    // Per spec section 9.2: a .md file with type: mds in frontmatter should compile
    let result = mds::compile(fixture("type_mds_md_file.md"), None).unwrap();
    assert!(
        result.contains("Hello World!"),
        "md file with type:mds should compile, got: {result}"
    );
}

#[test]
fn function_calls_function() {
    let result = mds::compile(fixture("fn_calls_fn.mds"), None).unwrap();
    assert!(result.contains("[Alice]"));
}

#[test]
fn recursion_detected() {
    let result = mds::compile(fixture("recursive.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("recursion"));
}

#[test]
fn nested_conditionals() {
    let result = mds::compile(fixture("nested_if.mds"), None).unwrap();
    assert!(result.contains("outer true"));
    assert!(result.contains("inner false"));
    assert!(!result.contains("inner true"));
    assert!(!result.contains("outer false"));
}

#[test]
fn vars_file_loading() {
    let dir = tempfile::tempdir().unwrap();
    let vars_path = dir.path().join("vars.json");
    std::fs::write(&vars_path, r#"{"name": "FromJSON", "count": 99}"#).unwrap();

    let vars = mds::load_vars_file(&vars_path).unwrap();
    assert_eq!(
        vars.get("name"),
        Some(&mds::Value::String("FromJSON".to_string()))
    );
    assert_eq!(vars.get("count"), Some(&mds::Value::Number(99.0)));
}

#[test]
fn check_valid_file() {
    let result = mds::check(fixture("simple.mds"), None);
    assert!(result.is_ok());
}

#[test]
fn check_invalid_file() {
    let result = mds::check(fixture("undefined_var.mds"), None);
    assert!(result.is_err());
}

#[test]
fn compile_file_compiles_valid_mds() {
    // compile_file is a thin wrapper over compile(); verify it produces correct output
    let path = fixture("simple.mds");
    let path_str = path.to_str().expect("fixture path is valid UTF-8");
    let result = mds::compile_file(path_str);
    assert!(
        result.is_ok(),
        "compile_file should succeed, got: {result:?}"
    );
    let output = result.unwrap();
    assert!(
        output.contains("Hello Alice!"),
        "compile_file output should contain 'Hello Alice!', got: {output}"
    );
}

#[test]
fn compile_file_returns_error_for_nonexistent_path() {
    let result = mds::compile_file("nonexistent_file_that_does_not_exist.mds");
    assert!(
        result.is_err(),
        "compile_file should fail for nonexistent file"
    );
    let err = result.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("nonexistent") || msg.contains("not found") || msg.contains("No such"),
        "error should describe the missing file, got: {msg}"
    );
}

// ── @if negation (!var) tests ────────────────────────────────────────────────

#[test]
fn if_negation_truthy_variable_skips_then_body() {
    // `@if !premium:` with premium=true → else branch executes
    let source = "---\npremium: true\n---\n@if !premium:\nfree_tier\n@else:\npaid_tier\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("paid_tier"),
        "negation of true must take else branch"
    );
    assert!(!result.contains("free_tier"), "then body must be skipped");
}

#[test]
fn if_negation_falsy_variable_enters_then_body() {
    // `@if !premium:` with premium=false → then branch executes
    let source = "---\npremium: false\n---\n@if !premium:\nfree_tier\n@else:\npaid_tier\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("free_tier"),
        "negation of false must take then branch"
    );
    assert!(!result.contains("paid_tier"), "else body must be skipped");
}

#[test]
fn if_negation_zero_is_truthy_branch() {
    // `@if !count:` with count=0 → 0 is falsy, so !0 is truthy
    let source = "---\ncount: 0\n---\n@if !count:\nzero_branch\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("zero_branch"),
        "negation of 0 must enter then branch"
    );
}

#[test]
fn if_negation_empty_string_is_truthy_branch() {
    // `@if !name:` with name="" → empty string is falsy, so !name is truthy
    let source = "---\nname: \"\"\n---\n@if !name:\nempty_branch\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("empty_branch"),
        "negation of empty string must enter then branch"
    );
}

#[test]
fn if_negation_null_is_truthy_branch() {
    // `@if !val:` with val=null → null is falsy, so !val is truthy
    let source = "---\nval: null\n---\n@if !val:\nnull_branch\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("null_branch"),
        "negation of null must enter then branch"
    );
}

#[test]
fn if_negation_dot_path() {
    // `@if !config.debug:` with config.debug=false → enters then branch
    let source = "---\nconfig:\n  debug: false\n---\n@if !config.debug:\ndebug_off_branch\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("debug_off_branch"),
        "negation of config.debug=false must enter then branch"
    );
}

// ── @if negation with undefined variable tests ───────────────────────────────

#[test]
fn if_negation_undefined_variable_is_error() {
    // `@if !undefined_var:` with no frontmatter defining undefined_var → error
    let source = "@if !undefined_var:\nbranch_body\n@end\n";
    let result = mds::compile_str(source);
    assert!(
        result.is_err(),
        "negation of undefined variable must produce an error, got: {:?}",
        result
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("undefined_var") || err.contains("undefined") || err.contains("not found"),
        "error should mention the undefined variable, got: {err}"
    );
}

// ── @if equality (==) tests ──────────────────────────────────────────────────

#[test]
fn if_eq_string_match_enters_then_body() {
    // `@if role == "admin":` with role=admin → then branch
    let source =
        "---\nrole: admin\n---\n@if role == \"admin\":\nadmin_yes\n@else:\nadmin_no\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("admin_yes"),
        "exact string match must enter then branch"
    );
    assert!(!result.contains("admin_no"), "else body must not appear");
}

#[test]
fn if_eq_string_no_match_enters_else_body() {
    // `@if role == "admin":` with role=user → else branch
    let source =
        "---\nrole: user\n---\n@if role == \"admin\":\nadmin_yes\n@else:\nadmin_no\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("admin_no"),
        "string mismatch must take else branch"
    );
    assert!(!result.contains("admin_yes"), "then body must not appear");
}

#[test]
fn if_eq_number_match() {
    // `@if count == 42:` with count=42 → then branch
    let source =
        "---\ncount: 42\n---\n@if count == 42:\ncount_match\n@else:\ncount_nomatch\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("count_match") && !result.contains("count_nomatch"),
        "number equality must match"
    );
}

#[test]
fn if_eq_bool_true_match() {
    // `@if active == true:` with active=true → then branch
    let source =
        "---\nactive: true\n---\n@if active == true:\nactive_on\n@else:\nactive_off\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("active_on") && !result.contains("active_off"),
        "bool true equality must match"
    );
}

#[test]
fn if_eq_null_match() {
    // `@if val == null:` with val=null → then branch
    let source = "---\nval: null\n---\n@if val == null:\nnull_matches\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(result.contains("null_matches"), "null equality must match");
}

#[test]
fn if_eq_strict_no_type_coercion_number_vs_string() {
    // `@if x == "3":` with x=3 (number) → strict, no coercion → else branch
    let source = "---\nx: 3\n---\n@if x == \"3\":\ncoercion_yes\n@else:\nstrict_types\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("strict_types"),
        "number 3 must not equal string \"3\""
    );
    assert!(!result.contains("coercion_yes"), "coercion must not happen");
}

#[test]
fn if_eq_strict_no_type_coercion_bool_vs_string() {
    // `@if x == "true":` with x=true (bool) → strict, no coercion → else branch
    let source =
        "---\nx: true\n---\n@if x == \"true\":\ncoercion_yes\n@else:\nstrict_types\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("strict_types"),
        "bool true must not equal string \"true\""
    );
    assert!(!result.contains("coercion_yes"), "coercion must not happen");
}

#[test]
fn if_eq_single_quoted_rhs() {
    // `@if role == 'admin':` with role=admin → then branch (single quotes)
    let source = "---\nrole: admin\n---\n@if role == 'admin':\nadmin_singlequote\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("admin_singlequote"),
        "single-quoted RHS must work"
    );
}

#[test]
fn if_eq_empty_string_rhs() {
    // `@if name == "":` with name="" → then branch
    let source = "---\nname: \"\"\n---\n@if name == \"\":\nempty_name_branch\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("empty_name_branch"),
        "empty string equality must match"
    );
}

#[test]
fn if_eq_operator_in_string_rhs() {
    // `@if msg == "a == b":` with msg="a == b" → then branch
    // The `==` inside the string must not be mistaken for the operator
    let source = "---\nmsg: \"a == b\"\n---\n@if msg == \"a == b\":\nop_inside_string_match\n@else:\nop_inside_string_nomatch\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("op_inside_string_match"),
        "operator inside string must not confuse the parser"
    );
    assert!(
        !result.contains("op_inside_string_nomatch"),
        "nomatch branch must not appear"
    );
}

#[test]
fn if_eq_negative_number() {
    // `@if temp == -5:` with temp=-5 → then branch
    let source = "---\ntemp: -5\n---\n@if temp == -5:\ncold_branch\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("cold_branch"),
        "negative number equality must match"
    );
}

#[test]
fn if_eq_float() {
    // `@if rate == 3.14:` with rate=3.14 → then branch
    let source = "---\nrate: 3.14\n---\n@if rate == 3.14:\nfloat_match\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(result.contains("float_match"), "float equality must match");
}

// ── @if inequality (!=) tests ────────────────────────────────────────────────

#[test]
fn if_neq_string_no_match_enters_then_body() {
    // `@if role != "admin":` with role=user → then branch
    let source = "---\nrole: user\n---\n@if role != \"admin\":\nnot_admin_branch\n@else:\nis_admin_branch\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("not_admin_branch"),
        "inequality with non-matching value must enter then branch"
    );
    assert!(!result.contains("is_admin_branch"), "else must not appear");
}

#[test]
fn if_neq_string_match_enters_else_body() {
    // `@if role != "admin":` with role=admin → else branch
    let source = "---\nrole: admin\n---\n@if role != \"admin\":\nnot_admin_branch\n@else:\nis_admin_branch\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("is_admin_branch"),
        "inequality with matching value must take else branch"
    );
    assert!(
        !result.contains("not_admin_branch"),
        "then branch must not appear"
    );
}

#[test]
fn if_neq_cross_type_always_true() {
    // `@if x != "3":` with x=3 (number) → types differ, always true
    let source =
        "---\nx: 3\n---\n@if x != \"3\":\ndiff_type_branch\n@else:\nsame_type_branch\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("diff_type_branch"),
        "cross-type != must be true"
    );
    assert!(!result.contains("same_type_branch"), "else must not appear");
}

// ── @elseif tests ───────────────────────────────────────────────────────────

#[test]
fn elseif_first_branch_matches() {
    // tier=enterprise → first branch matches
    let source = "---\ntier: enterprise\n---\n@if tier == \"enterprise\":\nenterprise_body\n@elseif tier == \"pro\":\npro_body\n@else:\nfree_body\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("enterprise_body"),
        "enterprise branch must match"
    );
    assert!(!result.contains("pro_body"), "pro branch must not appear");
    assert!(!result.contains("free_body"), "else branch must not appear");
}

#[test]
fn elseif_second_branch_matches() {
    // tier=pro → second branch matches
    let source = "---\ntier: pro\n---\n@if tier == \"enterprise\":\nenterprise_body\n@elseif tier == \"pro\":\npro_body\n@else:\nfree_body\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(result.contains("pro_body"), "pro branch must match");
    assert!(
        !result.contains("enterprise_body"),
        "enterprise branch must not appear"
    );
    assert!(!result.contains("free_body"), "else branch must not appear");
}

#[test]
fn elseif_falls_through_to_else() {
    // tier=starter → falls through to @else
    let source = "---\ntier: starter\n---\n@if tier == \"enterprise\":\nenterprise_body\n@elseif tier == \"pro\":\npro_body\n@else:\nfree_body\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("free_body"),
        "unmatched tiers must fall through to else"
    );
    assert!(
        !result.contains("enterprise_body"),
        "enterprise branch must not appear"
    );
    assert!(!result.contains("pro_body"), "pro branch must not appear");
}

#[test]
fn elseif_no_match_no_else_empty_output() {
    // No matching branch and no @else → only frontmatter in output
    let source = "---\ntier: starter\n---\n@if tier == \"enterprise\":\nenterprise_body\n@elseif tier == \"pro\":\npro_body\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        !result.contains("enterprise_body"),
        "unmatched must produce no enterprise output"
    );
    assert!(
        !result.contains("pro_body"),
        "unmatched must produce no pro output"
    );
}

#[test]
fn elseif_five_branches_matches_fourth() {
    // Five @elseif branches, match on branch 4
    let source = "---\nval: delta\n---\n@if val == \"alpha\":\nalpha_out\n@elseif val == \"bravo\":\nbravo_out\n@elseif val == \"charlie\":\ncharlie_out\n@elseif val == \"delta\":\ndelta_out\n@elseif val == \"echo\":\necho_out\n@else:\nother_out\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(result.contains("delta_out"), "fourth branch must match");
    assert!(
        !result.contains("alpha_out"),
        "alpha branch must not appear"
    );
    assert!(
        !result.contains("bravo_out"),
        "bravo branch must not appear"
    );
    assert!(
        !result.contains("charlie_out"),
        "charlie branch must not appear"
    );
    assert!(!result.contains("echo_out"), "echo branch must not appear");
    assert!(!result.contains("other_out"), "else branch must not appear");
}

#[test]
fn elseif_with_truthiness() {
    // `@if flag_a:` / `@elseif flag_b:` — truthiness conditions
    let source = "---\nflag_a: false\nflag_b: true\n---\n@if flag_a:\nbranch_a\n@elseif flag_b:\nbranch_b\n@else:\nbranch_none\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(result.contains("branch_b"), "elseif truthiness must work");
    assert!(!result.contains("branch_a"), "first branch must not appear");
}

#[test]
fn elseif_with_negation() {
    // `@if flag_x:` / `@elseif !flag_y:` — negation in elseif
    let source = "---\nflag_x: false\nflag_y: false\n---\n@if flag_x:\nbranch_x\n@elseif !flag_y:\nbranch_not_y\n@else:\nbranch_none\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("branch_not_y"),
        "negation in elseif must work"
    );
    assert!(!result.contains("branch_x"), "x branch must not appear");
}

#[test]
fn elseif_with_equality() {
    // `@elseif role == "mod":` — equality in elseif
    let source = "---\nrole: mod\n---\n@if role == \"admin\":\nadmin_body\n@elseif role == \"mod\":\nmod_body\n@else:\nother_body\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(result.contains("mod_body"), "equality in elseif must work");
    assert!(
        !result.contains("admin_body"),
        "admin branch must not appear"
    );
}

#[test]
fn elseif_nested_if_in_body() {
    // Nested @if inside @elseif body
    let source = "---\ntier: pro\nextra: true\n---\n@if tier == \"enterprise\":\nenterprise_body\n@elseif tier == \"pro\":\n@if extra:\npro_plus_body\n@else:\npro_basic_body\n@end\n@else:\nfree_body\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("pro_plus_body"),
        "nested @if inside elseif body must work"
    );
    assert!(
        !result.contains("enterprise_body"),
        "enterprise must not appear"
    );
}

#[test]
fn elseif_mixed_equality_and_inequality_in_chain() {
    // Chain: @if role == "admin" / @elseif role != "visitor" / @else
    // role=mod → first branch (==) fails, second branch (!=) is true → middle branch
    let source = "---\nrole: mod\n---\n\
        @if role == \"admin\":\nadmin_only\n\
        @elseif role != \"visitor\":\nregistered_user\n\
        @else:\nanon_visitor\n\
        @end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("registered_user"),
        "mod != visitor so != branch must match"
    );
    assert!(
        !result.contains("admin_only"),
        "admin branch must not appear"
    );
    assert!(
        !result.contains("anon_visitor"),
        "else branch must not appear"
    );

    // role=visitor → first branch (==) fails, second branch (!=) is false → else branch
    let source2 = "---\nrole: visitor\n---\n\
        @if role == \"admin\":\nadmin_only\n\
        @elseif role != \"visitor\":\nregistered_user\n\
        @else:\nanon_visitor\n\
        @end\n";
    let result2 = mds::compile_str(source2).unwrap();
    assert!(
        result2.contains("anon_visitor"),
        "visitor == visitor so != is false, must fall to else"
    );
    assert!(
        !result2.contains("registered_user"),
        "registered branch must not appear"
    );
    assert!(
        !result2.contains("admin_only"),
        "admin branch must not appear"
    );
}

#[test]
fn elseif_short_circuit_only_matched_branch_evaluates() {
    // Short-circuit: once a branch matches, subsequent @elseif are not evaluated.
    let source = "---\nval: first\n---\n@if val == \"first\":\nfirst_match_body\n@elseif val == \"first\":\nduplicate_match_body\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("first_match_body"),
        "first matching branch must win"
    );
    assert!(
        !result.contains("duplicate_match_body"),
        "subsequent branches must be skipped"
    );
}
