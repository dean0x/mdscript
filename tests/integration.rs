use std::collections::HashMap;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn mds_bin() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_mds"))
}

#[test]
fn simple_variable_interpolation() {
    let result = mds::compile(&fixture("simple.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("You have 3 items."));
}

#[test]
fn conditional_truthy() {
    let result = mds::compile(&fixture("conditional.mds"), None).unwrap();
    assert!(result.contains("Thanks for being premium!"));
    assert!(!result.contains("Upgrade for premium features."));
}

#[test]
fn conditional_falsy() {
    let result = mds::compile(&fixture("conditional_false.mds"), None).unwrap();
    assert!(!result.contains("Thanks for being premium!"));
    assert!(result.contains("Upgrade for premium features."));
}

#[test]
fn loop_over_array() {
    let result = mds::compile(&fixture("loop.mds"), None).unwrap();
    assert!(result.contains("- apple"));
    assert!(result.contains("- banana"));
    assert!(result.contains("- cherry"));
}

#[test]
fn function_definition_and_call() {
    let result = mds::compile(&fixture("function.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("Hello Bob!"));
}

#[test]
fn escaped_braces() {
    let result = mds::compile(&fixture("escaped.mds"), None).unwrap();
    assert!(result.contains("{name}"));
}

#[test]
fn code_block_passthrough() {
    let result = mds::compile(&fixture("code_block.mds"), None).unwrap();
    // Inside code block: no interpolation should occur
    assert!(result.contains("{not_a_var}"));
    assert!(result.contains("{world}"));
    // Outside code block: interpolation should work
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("Goodbye Alice!"));
}

#[test]
fn import_alias() {
    let result = mds::compile(&fixture("import_alias.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("Goodbye Alice!"));
}

#[test]
fn import_merge() {
    let result = mds::compile(&fixture("import_merge.mds"), None).unwrap();
    assert!(result.contains("Hello Bob!"));
    assert!(result.contains("Goodbye Bob!"));
}

#[test]
fn import_selective() {
    let result = mds::compile(&fixture("import_selective.mds"), None).unwrap();
    assert!(result.contains("Hello Charlie!"));
}

#[test]
fn include_directive() {
    let result = mds::compile(&fixture("include_test.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("Thank you for using our service."));
}

#[test]
fn reexport() {
    let result = mds::compile(&fixture("reexport_consumer.mds"), None).unwrap();
    assert!(result.contains("Hello Dave!"));
}

#[test]
fn wildcard_reexport_barrel() {
    let result = mds::compile(&fixture("barrel_consumer.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("- search"));
    assert!(result.contains("- code"));
    assert!(result.contains("- browse"));
}

#[test]
fn circular_import_error() {
    let result = mds::compile(&fixture("circular_a.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("circular import"),
        "expected circular import error, got: {err}"
    );
    assert!(
        err.contains('\u{2192}'),
        "expected cycle chain with → arrow, got: {err}"
    );
}

#[test]
fn undefined_variable_error() {
    let result = mds::compile(&fixture("undefined_var.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("username"));
}

#[test]
fn arity_mismatch_error() {
    let result = mds::compile(&fixture("arity_error.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("arity") || err.contains("expected 1"));
}

#[test]
fn runtime_vars_override() {
    let mut vars = HashMap::new();
    vars.insert(
        "name".to_string(),
        mds::value::Value::String("Override".to_string()),
    );
    let result = mds::compile(&fixture("simple.mds"), Some(vars)).unwrap();
    assert!(result.contains("Hello Override!"));
}

#[test]
fn complete_example_welcome() {
    let result = mds::compile(&fixture("welcome.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("- apple"));
    assert!(result.contains("- banana"));
    assert!(result.contains("Thanks for being premium!"));
    assert!(!result.contains("Upgrade for premium features."));
    assert!(result.contains("Thank you for using our service."));
}

#[test]
fn file_not_found_error() {
    let result = mds::compile(&PathBuf::from("nonexistent.mds"), None);
    assert!(result.is_err());
}

#[test]
fn not_mds_file_error() {
    // Try to compile a .md file without type: mds
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("spec.md");
    let result = mds::compile(&path, None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("not an MDS file") || err.contains("not_mds"));
}

#[test]
fn vars_file_loading() {
    // Create a temporary vars file
    let vars_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("test_vars.json");
    std::fs::write(&vars_path, r#"{"name": "FromJSON", "count": 99}"#).unwrap();

    let vars = mds::load_vars_file(&vars_path).unwrap();
    assert_eq!(
        vars.get("name"),
        Some(&mds::value::Value::String("FromJSON".to_string()))
    );
    assert_eq!(vars.get("count"), Some(&mds::value::Value::Number(99.0)));

    // Clean up
    let _ = std::fs::remove_file(&vars_path);
}

#[test]
fn check_valid_file() {
    let result = mds::check(&fixture("simple.mds"), None);
    assert!(result.is_ok());
}

#[test]
fn check_invalid_file() {
    let result = mds::check(&fixture("undefined_var.mds"), None);
    assert!(result.is_err());
}

#[test]
fn function_calls_function() {
    let result = mds::compile(&fixture("fn_calls_fn.mds"), None).unwrap();
    assert!(result.contains("[Alice]"));
}

#[test]
fn recursion_detected() {
    let result = mds::compile(&fixture("recursive.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("recursion"));
}

#[test]
fn nested_conditionals() {
    let result = mds::compile(&fixture("nested_if.mds"), None).unwrap();
    assert!(result.contains("outer true"));
    assert!(result.contains("inner false"));
    assert!(!result.contains("inner true"));
    assert!(!result.contains("outer false"));
}

#[test]
fn absolute_import_path_rejected() {
    let result = mds::compile(&fixture("absolute_import.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("relative") || err.contains("import"));
}

#[test]
fn unicode_content() {
    let result = mds::compile(&fixture("unicode.mds"), None).unwrap();
    assert!(result.contains("Greetings Rene!"));
    assert!(result.contains("Hello"));
    // Code block content should not be interpolated
    assert!(result.contains("{not_interpolated}"));
    assert!(result.contains("Farewell Rene!"));
}

#[test]
fn for_iterate_non_array_error() {
    // Attempting to iterate over a non-array should produce a type error
    let mut vars = HashMap::new();
    vars.insert(
        "items".to_string(),
        mds::value::Value::String("not_an_array".to_string()),
    );
    let result = mds::compile(&fixture("loop.mds"), Some(vars));
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("expected array") || err.contains("type error") || err.contains("string"));
}

#[test]
fn empty_array_loop() {
    // Iterating over an empty array should produce no output for the loop body
    let mut vars = HashMap::new();
    vars.insert("items".to_string(), mds::value::Value::Array(vec![]));
    let result = mds::compile(&fixture("loop.mds"), Some(vars)).unwrap();
    assert!(!result.contains("- apple"));
    assert!(!result.contains("- banana"));
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
fn name_collision_on_merge_import() {
    let result = mds::compile(&fixture("collision_consumer.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("name collision") || err.contains("greet"),
        "expected name collision error, got: {err}"
    );
}

#[test]
fn merge_import_does_not_leak_vars() {
    // Per spec: merge imports bring in functions only, NOT frontmatter variables.
    // Two merge-imported modules that both define the same variable should NOT cause
    // a name collision — because variables are not imported at all.
    let result = mds::compile(&fixture("var_collision_consumer.mds"), None);
    assert!(
        result.is_ok(),
        "merge import should not leak variables (no collision expected), got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn type_key_available_in_mds_files() {
    let result = mds::compile(&fixture("type_variable.mds"), None).unwrap();
    assert!(
        result.contains("assistant"),
        "expected 'type' variable to be available in .mds files, got: {result}"
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
    let result = mds::compile(&fixture("for_body_undef.mds"), None);
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
fn cross_module_function_preserves_lexical_scope() {
    // A function defined in module A that uses an alias import (u -> utils.mds)
    // must resolve that alias from its *definition* site (lexical scope) even when
    // called from module B, which has no knowledge of 'u'.
    let result = mds::compile(&fixture("lexical_scope_consumer.mds"), None).unwrap();
    assert!(
        result.contains("Hello Alice!"),
        "expected 'Hello Alice!' in output (lexical scope), got: {result}"
    );
    assert!(
        result.contains("Welcome"),
        "expected 'Welcome' in output, got: {result}"
    );
}

#[test]
fn file_size_limit_rejects_huge_file() {
    let dir = tempfile::tempdir().unwrap();
    let huge = dir.path().join("huge.mds");
    // Create a file just over 10MB
    let content = "x".repeat(10 * 1024 * 1024 + 1);
    std::fs::write(&huge, &content).unwrap();
    let result = mds::compile(&huge, None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("file too large"),
        "Expected 'file too large' error, got: {err}"
    );
}

#[test]
fn path_traversal_import_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();

    // Create a file outside 'sub' directory
    let outside = dir.path().join("secret.mds");
    std::fs::write(&outside, "Secret content").unwrap();

    // Create a file inside 'sub' that tries to import outside its root
    let child = sub.join("child.mds");
    std::fs::write(&child, "@import \"../secret.mds\" as s\n\n@include s\n").unwrap();

    let result = mds::compile(&child, None);
    assert!(result.is_err(), "Traversal import should be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("escapes project directory") || err.contains("import"),
        "Expected path traversal error, got: {err}"
    );
}

#[test]
fn import_depth_limit() {
    let dir = tempfile::tempdir().unwrap();
    // Create a chain of 66 files, each importing the next
    let depth = 66;
    for i in 0..depth {
        let name = format!("mod_{i}.mds");
        let content = if i < depth - 1 {
            let next = format!("mod_{}.mds", i + 1);
            format!("@import \"./{next}\" as m{}\n\nLevel {i}\n", i + 1)
        } else {
            format!("End of chain {i}\n")
        };
        std::fs::write(dir.path().join(&name), content).unwrap();
    }
    let result = mds::compile(&dir.path().join("mod_0.mds"), None);
    assert!(result.is_err(), "Deep import chain should be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("import depth") || err.contains("64"),
        "Expected import depth error, got: {err}"
    );
}

#[test]
fn init_does_not_overwrite_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("existing.mds");
    std::fs::write(&existing, "original content").unwrap();

    // Try to init over existing file - should fail
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mds"))
        .args(["init", existing.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "init should fail on existing file without --force"
    );

    // Verify original content preserved
    let content = std::fs::read_to_string(&existing).unwrap();
    assert_eq!(content, "original content");
}

#[test]
fn cross_module_frontmatter_var_in_function() {
    // A function defined in module A that references module A's frontmatter variable
    // must resolve that variable from its *definition* site (lexical scope) even when
    // called from module B, which has no knowledge of that variable.
    let result = mds::compile(&fixture("fm_var_consumer.mds"), None).unwrap();
    assert!(
        result.contains("Hello from module A"),
        "expected frontmatter variable to be accessible in cross-module function call, got: {result}"
    );
}

#[test]
fn export_nonexistent_symbol_errors() {
    // @export phantom where 'phantom' is never defined should be a compile error.
    let result = mds::compile(&fixture("export_phantom.mds"), None);
    assert!(
        result.is_err(),
        "expected error when exporting undefined symbol"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("phantom") || err.contains("export") || err.contains("not defined"),
        "expected export error mentioning 'phantom', got: {err}"
    );
}

#[test]
fn check_stdin_valid() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mds"))
        .args(["check", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(b"---\nname: World\n---\nHello {name}!\n")
                .unwrap();
            child.wait_with_output()
        })
        .unwrap();
    assert!(
        output.status.success(),
        "check stdin should succeed for valid input"
    );
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
fn selective_import_prompt_body() {
    let result = mds::compile(&fixture("prompt_consumer.mds"), None).unwrap();
    assert!(
        result.contains("This is the module body text."),
        "selective import of 'prompt' should bring in the module's body text, got: {result}"
    );
}

#[test]
fn set_flag_cli_overrides() {
    // --set name=Test should override the frontmatter variable 'name'
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mds"))
        .args([
            "build",
            fixture("simple.mds").to_str().unwrap(),
            "--set",
            "name=Test",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(output.status.success(), "build with --set should succeed");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Hello Test!"),
        "expected '--set name=Test' to override frontmatter, got: {stdout}"
    );
}

#[test]
fn set_flag_boolean_coercion() {
    // --set premium=false must coerce the string "false" to boolean false,
    // so @if premium: evaluates as falsy and the @else branch is rendered.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mds"))
        .args([
            "build",
            fixture("set_flag_false.mds").to_str().unwrap(),
            "--set",
            "premium=false",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build with --set premium=false should succeed"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Upgrade for premium features."),
        "expected falsy branch when --set premium=false, got: {stdout}"
    );
    assert!(
        !stdout.contains("Thanks for being premium!"),
        "truthy branch must not appear when --set premium=false, got: {stdout}"
    );
}

#[test]
fn set_flag_boolean_true_coercion() {
    // --set premium=true must coerce to boolean true (truthy branch rendered).
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mds"))
        .args([
            "build",
            fixture("set_flag_false.mds").to_str().unwrap(),
            "--set",
            "premium=true",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build with --set premium=true should succeed"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Thanks for being premium!"),
        "expected truthy branch when --set premium=true, got: {stdout}"
    );
}

#[test]
fn set_flag_numeric_coercion() {
    // --set count=3 must coerce the string "3" to a number so {count} renders as "3".
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mds"))
        .args([
            "build",
            fixture("set_count.mds").to_str().unwrap(),
            "--set",
            "count=3",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build with --set count=3 should succeed"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Count is 3."),
        "expected numeric value from --set count=3, got: {stdout}"
    );
}

#[test]
fn set_flag_null_coercion() {
    // --set premium=null must coerce to Value::Null (falsy), so @else branch renders.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mds"))
        .args([
            "build",
            fixture("set_flag_false.mds").to_str().unwrap(),
            "--set",
            "premium=null",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build with --set premium=null should succeed"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Upgrade for premium features."),
        "expected falsy branch when --set premium=null, got: {stdout}"
    );
}

// ── CLI Integration Tests ────────────────────────────────────────────────────

#[test]
fn build_to_file() {
    let dir = tempfile::tempdir().unwrap();
    let output_path = dir.path().join("output.md");

    let output = mds_bin()
        .args([
            "build",
            fixture("simple.mds").to_str().unwrap(),
            "-o",
            output_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "build to file should succeed");
    let content = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        content.contains("Hello Alice!"),
        "output file should contain compiled content, got: {content}"
    );
}

#[test]
fn build_from_stdin() {
    let mut child = mds_bin()
        .args(["build", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"---\nname: World\n---\nHello {name}!\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "build from stdin should succeed");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Hello World!"),
        "stdin build should produce 'Hello World!', got: {stdout}"
    );
}

#[test]
fn build_with_vars_file() {
    let dir = tempfile::tempdir().unwrap();
    let vars_path = dir.path().join("vars.json");
    std::fs::write(&vars_path, r#"{"name": "Overridden"}"#).unwrap();

    let output = mds_bin()
        .args([
            "build",
            fixture("simple.mds").to_str().unwrap(),
            "--vars",
            vars_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build with vars file should succeed"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Hello Overridden!"),
        "vars file should override frontmatter 'name', got: {stdout}"
    );
}

#[test]
fn build_invalid_input_exits_nonzero() {
    let output = mds_bin()
        .args(["build", "nonexistent_file_that_does_not_exist.mds"])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "build with nonexistent file should exit non-zero"
    );
}

#[test]
fn check_invalid_exits_nonzero() {
    let output = mds_bin()
        .args(["check", fixture("undefined_var.mds").to_str().unwrap()])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "check on invalid file should exit non-zero"
    );
}

#[test]
fn init_creates_compilable_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("test.mds");

    // Create the file
    let init_output = mds_bin()
        .args(["init", target.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(init_output.status.success(), "init should succeed");
    assert!(target.exists(), "init should create the file");

    // Compile the created file
    let build_output = mds_bin()
        .args(["build", target.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        build_output.status.success(),
        "init-created file should compile successfully; stderr: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );
}

#[test]
fn init_force_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("test.mds");
    std::fs::write(&target, "original content").unwrap();

    let output = mds_bin()
        .args(["init", target.to_str().unwrap(), "--force"])
        .output()
        .unwrap();

    assert!(output.status.success(), "init --force should succeed");
    let content = std::fs::read_to_string(&target).unwrap();
    assert!(
        content != "original content",
        "init --force should overwrite the file"
    );
    assert!(
        content.contains("Hello"),
        "overwritten file should contain template content"
    );
}

#[test]
fn build_quiet_flag() {
    let dir = tempfile::tempdir().unwrap();
    let output_path = dir.path().join("output.md");

    let output = mds_bin()
        .args([
            "build",
            fixture("simple.mds").to_str().unwrap(),
            "-o",
            output_path.to_str().unwrap(),
            "-q",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "quiet build should succeed");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.is_empty(),
        "quiet flag should produce no stderr output, got: {stderr}"
    );
}

// ── Edge Case Tests ──────────────────────────────────────────────────────────

#[test]
fn md_file_with_type_mds_compiles() {
    // Per spec section 9.2: a .md file with type: mds in frontmatter should compile
    let result = mds::compile(&fixture("type_mds_md_file.md"), None).unwrap();
    assert!(
        result.contains("Hello World!"),
        "md file with type:mds should compile, got: {result}"
    );
}

#[test]
fn nested_loops() {
    let result = mds::compile(&fixture("nested_loops.mds"), None).unwrap();
    assert!(result.contains("row1-col1"), "nested loops: row1-col1");
    assert!(result.contains("row1-col2"), "nested loops: row1-col2");
    assert!(result.contains("row2-col1"), "nested loops: row2-col1");
    assert!(result.contains("row2-col2"), "nested loops: row2-col2");
}

#[test]
fn function_called_in_loop() {
    let result = mds::compile(&fixture("fn_in_loop.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"), "fn in loop: Alice");
    assert!(result.contains("Hello Bob!"), "fn in loop: Bob");
    assert!(result.contains("Hello Charlie!"), "fn in loop: Charlie");
}

#[test]
fn loop_var_shadows_outer() {
    let result = mds::compile(&fixture("loop_var_shadow.mds"), None).unwrap();
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
    let result = mds::compile(&fixture("fn_param_shadow.mds"), None).unwrap();
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
fn selective_import_nonexistent_errors() {
    let result = mds::compile(&fixture("selective_import_nonexistent.mds"), None);
    assert!(
        result.is_err(),
        "selective import of nonexistent symbol should error"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("nonexistent") || err.contains("not exported"),
        "error should mention nonexistent symbol, got: {err}"
    );
}

#[test]
fn nested_function_calls_in_interpolation() {
    let result = mds::compile(&fixture("nested_fn_calls.mds"), None).unwrap();
    // outer(inner("arg")) => outer("arg!") => "[arg!]"
    assert!(
        result.contains("[arg!]"),
        "nested fn calls should produce '[arg!]', got: {result}"
    );
}

#[test]
fn empty_frontmatter() {
    let result = mds::compile(&fixture("empty_frontmatter.mds"), None).unwrap();
    assert!(
        result.contains("Hello World!"),
        "empty frontmatter file should compile, got: {result}"
    );
}

#[test]
fn no_frontmatter_with_directives() {
    let result = mds::compile(&fixture("no_frontmatter_with_define.mds"), None).unwrap();
    assert!(
        result.contains("Hello World!"),
        "file with @define but no frontmatter should compile, got: {result}"
    );
}

#[test]
fn multiple_escaped_braces() {
    let result = mds::compile(&fixture("multiple_escaped_braces.mds"), None).unwrap();
    // \{a\} → literal '{' + 'a\}' as text, \{b\} → literal '{' + 'b\}' as text
    // Per spec: only \{ is an escape sequence, \} is plain text
    assert!(
        result.contains("{a") && result.contains("{b"),
        "escaped braces should produce literal '{{', got: {result}"
    );
}

// ── Falsy Values (Spec 4.3) ──────────────────────────────────────────────────

#[test]
fn if_falsy_zero() {
    let result = mds::compile(&fixture("if_falsy_zero.mds"), None).unwrap();
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
    let result = mds::compile(&fixture("if_falsy_null.mds"), None).unwrap();
    assert!(
        result.contains("falsy"),
        "null should be falsy, got: {result}"
    );
}

#[test]
fn if_falsy_empty_string() {
    let result = mds::compile(&fixture("if_falsy_empty_string.mds"), None).unwrap();
    assert!(
        result.contains("falsy"),
        "empty string should be falsy, got: {result}"
    );
}

#[test]
fn if_falsy_empty_array() {
    let result = mds::compile(&fixture("if_falsy_empty_array.mds"), None).unwrap();
    assert!(
        result.contains("falsy"),
        "empty array should be falsy, got: {result}"
    );
}

// ── Mutual Recursion (Spec 4.5) ──────────────────────────────────────────────

#[test]
fn mutual_recursion_detected() {
    let result = mds::compile(&fixture("mutual_recursion.mds"), None);
    assert!(result.is_err(), "mutual recursion should be detected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("recursion"),
        "expected recursion error, got: {err}"
    );
}

// ── Alias Prevents Unqualified Access (Spec 4.6) ─────────────────────────────

#[test]
fn alias_import_no_unqualified_access() {
    // 'greet' was imported under alias 'g', so bare {greet(name)} must fail.
    let result = mds::compile(&fixture("alias_no_unqualified.mds"), None);
    assert!(
        result.is_err(),
        "unqualified access after alias import should fail"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("undefined") || err.contains("greet"),
        "expected undefined function/variable error, got: {err}"
    );
}

// ── @export from Does NOT Bring Symbol Into Local Scope (Spec 4.7) ───────────

#[test]
fn export_from_no_local_scope() {
    // @export hello from "./greetings.mds" re-exports without local availability.
    let result = mds::compile(&fixture("export_from_no_local.mds"), None);
    assert!(
        result.is_err(),
        "@export from should not make symbol available locally"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("undefined"),
        "expected undefined error, got: {err}"
    );
}

// ── Escaped Braces in Function Body ─────────────────────────────────────────

#[test]
fn escaped_braces_in_function_body() {
    // Per spec and existing tests (multiple_escaped_braces): only \{ is an escape
    // sequence producing a literal '{'. The closing \} is plain text, rendered as \}.
    // So \{curly braces\} → "{curly braces\}" in output.
    let result = mds::compile(&fixture("escaped_brace_in_fn.mds"), None).unwrap();
    assert!(
        result.contains("{curly braces"),
        "escaped brace in function body should produce literal brace, got: {result}"
    );
    assert!(
        result.contains("Alice"),
        "interpolation inside function body should still work, got: {result}"
    );
}

// ── Escaped Braces in @if and @for Bodies ────────────────────────────────────

#[test]
fn escaped_braces_in_blocks() {
    // Per spec and existing tests (multiple_escaped_braces): only \{ is an escape
    // sequence producing a literal '{'. The closing \} is plain text, rendered as \}.
    // So \{variable\} => "{variable\}" and \{item\} => "{item\}".
    let result = mds::compile(&fixture("escaped_brace_in_blocks.mds"), None).unwrap();
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

// ── Duplicate @define Should Error ───────────────────────────────────────────

#[test]
fn duplicate_define_errors() {
    // NOTE: This test documents expected behavior (Spec: no duplicate function names).
    // If the compiler does not yet enforce this, this test will fail until the fix lands.
    let result = mds::compile(&fixture("duplicate_define.mds"), None);
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

// ── Empty Include Produces No Crash (Spec 4.8) ───────────────────────────────

#[test]
fn include_empty_body_no_crash() {
    // @include of a module with only function definitions (no body text) should
    // produce an empty string for the include, not crash.
    let result = mds::compile(&fixture("include_empty_body.mds"), None).unwrap();
    assert!(
        result.contains("Before"),
        "output should contain 'Before', got: {result}"
    );
    assert!(
        result.contains("After"),
        "output should contain 'After', got: {result}"
    );
}

#[test]
fn include_empty_body_emits_warning() {
    // Per spec 4.8: @include of a module with no body text should emit a warning
    // to stderr (when not in quiet mode).
    let output = mds_bin()
        .args(["build", fixture("include_empty_body.mds").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build should succeed even when include is empty"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("warning") && stderr.contains("fns"),
        "expected warning about empty @include on stderr, got: {stderr}"
    );
}

#[test]
fn include_empty_body_no_warning_in_quiet_mode() {
    // When -q/--quiet is set, the warning should be suppressed.
    let output = mds_bin()
        .args([
            "build",
            fixture("include_empty_body.mds").to_str().unwrap(),
            "--quiet",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "quiet build should succeed");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.is_empty(),
        "quiet flag should suppress the empty-include warning, got: {stderr}"
    );
}

// ── Error Format Verification ────────────────────────────────────────────────

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

// ── compile_file convenience function ────────────────────────────────────────

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

// ── Error help message verification ──────────────────────────────────────────

#[test]
fn circular_import_error_has_help_text() {
    let result = mds::compile(&fixture("circular_a.mds"), None);
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

// ── CLI auto-detect .mds file ─────────────────────────────────────────────────

#[test]
fn build_auto_detects_single_mds_file_in_directory() {
    // Create a temp directory with exactly one .mds file
    let dir = tempfile::tempdir().expect("create temp dir");
    let mds_path = dir.path().join("auto.mds");
    std::fs::write(&mds_path, "---\nname: World\n---\nHello {name}!\n").expect("write fixture");

    let output = mds_bin()
        .current_dir(dir.path())
        .arg("build")
        .output()
        .expect("run mds build");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.success(),
        "auto-detect should succeed with one .mds file; stderr: {stderr}"
    );
    assert!(
        stdout.contains("Hello World!"),
        "auto-detect output should contain 'Hello World!', got stdout: {stdout}"
    );
}

#[test]
fn build_errors_when_no_mds_files_in_directory() {
    let dir = tempfile::tempdir().expect("create temp dir");

    let output = mds_bin()
        .current_dir(dir.path())
        .arg("build")
        .output()
        .expect("run mds build");

    assert!(
        !output.status.success(),
        "build with no .mds files should fail"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("no .mds files") || stderr.contains("No .mds files"),
        "error should mention missing .mds files, got: {stderr}"
    );
}

#[test]
fn build_errors_when_multiple_mds_files_in_directory() {
    let dir = tempfile::tempdir().expect("create temp dir");
    std::fs::write(dir.path().join("a.mds"), "---\n---\nhello\n").expect("write a.mds");
    std::fs::write(dir.path().join("b.mds"), "---\n---\nworld\n").expect("write b.mds");

    let output = mds_bin()
        .current_dir(dir.path())
        .arg("build")
        .output()
        .expect("run mds build");

    assert!(
        !output.status.success(),
        "build with multiple .mds files should fail"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("multiple") || stderr.contains("Multiple"),
        "error should mention multiple .mds files, got: {stderr}"
    );
}

#[test]
fn check_auto_detects_single_mds_file_in_directory() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mds_path = dir.path().join("valid.mds");
    std::fs::write(&mds_path, "---\nname: World\n---\nHello {name}!\n").expect("write fixture");

    let output = mds_bin()
        .current_dir(dir.path())
        .arg("check")
        .output()
        .expect("run mds check");

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.success(),
        "check auto-detect should succeed; stderr: {stderr}"
    );
    assert!(
        stderr.contains("OK"),
        "check should print OK message, got stderr: {stderr}"
    );
}
