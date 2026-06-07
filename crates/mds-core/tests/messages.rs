//! Integration tests for the `@message` block directive and `compile_messages_*` API.
//!
//! These tests cover:
//! - AC-1: bare-word role parsing (`@message system:`)
//! - AC-2: dynamic role expression (`@message {role}:`)
//! - AC-3: text mode backward-compatibility (body rendered inline)
//! - AC-4: error cases (no messages, nested messages, empty role)
//! - AC-5: control-flow inside @message (interpolation, @for, @if)
//! - AC-6: multi-message templates

use std::collections::HashMap;

use mds::{compile_messages_str, compile_messages_virtual, compile_messages_virtual_with_deps};

// ── AC-1: bare-word role → "system" literal ───────────────────────────────────

#[test]
fn bare_word_system_role() {
    let result = compile_messages_str("@message system:\nYou are a helpful assistant.\n@end\n")
        .expect("should compile");
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].role, "system");
    assert_eq!(result.messages[0].content, "You are a helpful assistant.");
}

#[test]
fn bare_word_user_role() {
    let result =
        compile_messages_str("@message user:\nWhat is Rust?\n@end\n").expect("should compile");
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].role, "user");
    assert_eq!(result.messages[0].content, "What is Rust?");
}

#[test]
fn bare_word_assistant_role() {
    let result =
        compile_messages_str("@message assistant:\nI am ready.\n@end\n").expect("should compile");
    assert_eq!(result.messages[0].role, "assistant");
    assert_eq!(result.messages[0].content, "I am ready.");
}

// AC-1 guard: bare-word role must NOT perform variable lookup
#[test]
fn bare_word_role_is_literal_not_variable_lookup() {
    // Even if `system` is defined as a variable in frontmatter, the bare-word
    // role in `@message system:` must still produce "system" (not the var value).
    let src = "---\nsystem: injected\n---\n@message system:\nBody.\n@end\n";
    let result = compile_messages_str(src).expect("should compile");
    assert_eq!(
        result.messages[0].role, "system",
        "bare-word role must not look up variables; got: {}",
        result.messages[0].role
    );
}

// ── AC-2: dynamic role expression ─────────────────────────────────────────────

#[test]
fn dynamic_role_from_variable() {
    let src = "---\nrole: assistant\n---\n@message {role}:\nHello!\n@end\n";
    let result = compile_messages_str(src).expect("should compile");
    assert_eq!(result.messages[0].role, "assistant");
}

#[test]
fn dynamic_role_from_runtime_var() {
    use std::collections::HashMap;
    let vars = HashMap::from([("r".to_string(), mds::Value::String("user".to_string()))]);
    let result =
        mds::compile_messages_str_with("@message {r}:\nAsk something.\n@end\n", None, Some(vars))
            .expect("should compile");
    assert_eq!(result.messages[0].role, "user");
}

#[test]
fn dynamic_role_non_string_type_errors() {
    let src = "---\ncount: 42\n---\n@message {count}:\nBody.\n@end\n";
    let err = compile_messages_str(src).expect_err("non-string role must error");
    let msg = err.to_string();
    assert!(
        msg.contains("role must evaluate to a string") || msg.contains("type"),
        "expected type error, got: {msg}"
    );
}

// ── AC-3: text mode backward-compatibility ────────────────────────────────────

#[test]
fn text_mode_renders_body_inline() {
    // In normal compile_str mode @message is transparent — body renders inline.
    let src = "@message system:\nHello world.\n@end\n";
    let output = mds::compile_str(src).expect("should compile in text mode");
    assert!(
        output.contains("Hello world."),
        "body must appear in text output; got: {output:?}"
    );
    // The @message markers must not appear in text output
    assert!(
        !output.contains("@message"),
        "markers must not appear in text output; got: {output:?}"
    );
}

#[test]
fn text_mode_multiple_messages_renders_all_bodies() {
    let src = "@message system:\nSys.\n@end\n@message user:\nUser.\n@end\n";
    let output = mds::compile_str(src).expect("should compile in text mode");
    assert!(output.contains("Sys."), "got: {output:?}");
    assert!(output.contains("User."), "got: {output:?}");
}

// ── AC-4: error cases ─────────────────────────────────────────────────────────

#[test]
fn no_message_blocks_is_error() {
    let err = compile_messages_str("Hello world!\n")
        .expect_err("template with no @message blocks must error");
    let msg = err.to_string();
    assert!(
        msg.contains("no @message") || msg.contains("at least one"),
        "expected 'no @message blocks' error, got: {msg}"
    );
}

#[test]
fn nested_message_blocks_is_parse_error() {
    let src = "@message system:\n@message user:\nNested.\n@end\n@end\n";
    let err = compile_messages_str(src).expect_err("nested @message must be a parse error");
    let msg = err.to_string();
    assert!(
        msg.contains("nested") || msg.contains("cannot be nested"),
        "expected nesting error, got: {msg}"
    );
}

#[test]
fn empty_role_is_parse_error() {
    // @message with an empty role after stripping the colon must fail.
    let err = compile_messages_str("@message :\nBody.\n@end\n").expect_err("empty role must error");
    let msg = err.to_string();
    assert!(
        msg.contains("role") || msg.contains("empty") || msg.contains("@message"),
        "expected role error, got: {msg}"
    );
}

#[test]
fn empty_body_message_is_skipped() {
    // An @message block whose body trims to empty is silently dropped.
    let src = "@message system:\n   \n@end\n@message user:\nContent.\n@end\n";
    let result = compile_messages_str(src).expect("should compile");
    assert_eq!(
        result.messages.len(),
        1,
        "empty body should be skipped; got: {:#?}",
        result.messages
    );
    assert_eq!(result.messages[0].role, "user");
}

// ── AC-5: control-flow within @message ───────────────────────────────────────

#[test]
fn interpolation_inside_message_body() {
    let src = "---\nname: Alice\n---\n@message user:\nHello {name}!\n@end\n";
    let result = compile_messages_str(src).expect("should compile");
    assert_eq!(result.messages[0].content, "Hello Alice!");
}

#[test]
fn if_block_inside_message_body() {
    let src = concat!(
        "---\nadmin: true\n---\n",
        "@message system:\n",
        "@if admin:\nAdmin mode.\n@else:\nUser mode.\n@end\n",
        "@end\n",
    );
    let result = compile_messages_str(src).expect("should compile");
    assert!(
        result.messages[0].content.contains("Admin mode."),
        "got: {:?}",
        result.messages[0].content
    );
}

#[test]
fn for_loop_inside_message_body() {
    let src = concat!(
        "---\nitems:\n  - a\n  - b\n---\n",
        "@message user:\n",
        "@for item in items:\n{item}\n@end\n",
        "@end\n",
    );
    let result = compile_messages_str(src).expect("should compile");
    assert!(result.messages[0].content.contains("a"));
    assert!(result.messages[0].content.contains("b"));
}

// ── AC-6: multi-message templates ────────────────────────────────────────────

#[test]
fn multiple_messages_preserve_order() {
    let src = concat!(
        "@message system:\nSystem prompt.\n@end\n",
        "@message user:\nUser question.\n@end\n",
        "@message assistant:\nAssistant reply.\n@end\n",
    );
    let result = compile_messages_str(src).expect("should compile");
    assert_eq!(result.messages.len(), 3);
    assert_eq!(result.messages[0].role, "system");
    assert_eq!(result.messages[1].role, "user");
    assert_eq!(result.messages[2].role, "assistant");
}

#[test]
fn messages_mode_warns_on_orphan_text() {
    // Text outside @message blocks must emit a warning in messages mode.
    let src = "Orphan text.\n@message user:\nQuestion.\n@end\n";
    let result = compile_messages_str(src).expect("should compile despite orphan text");
    assert!(
        !result.warnings.is_empty(),
        "expected warning for orphan text; got none"
    );
    let has_orphan_warn = result
        .warnings
        .iter()
        .any(|w| w.contains("outside @message") || w.contains("orphan") || w.contains("ignored"));
    assert!(
        has_orphan_warn,
        "expected orphan-text warning; got: {:#?}",
        result.warnings
    );
}

// ── Dependencies tracking ─────────────────────────────────────────────────────

#[test]
fn compile_messages_virtual_with_deps_excludes_entry() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "@message system:\nHello.\n@end\n".to_string(),
    );
    let result =
        compile_messages_virtual_with_deps(modules, "main.mds", None).expect("should compile");
    // Entry is excluded from dependencies
    assert!(
        !result.dependencies.contains(&"main.mds".to_string()),
        "entry should be excluded from dependencies; got: {:#?}",
        result.dependencies
    );
}

#[test]
fn compile_messages_virtual_no_message_blocks_errors() {
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), "Hello world!\n".to_string());
    let err = compile_messages_virtual(modules, "main.mds", None)
        .expect_err("template with no @message blocks must error");
    let msg = err.to_string();
    assert!(
        msg.contains("no @message") || msg.contains("at least one"),
        "got: {msg}"
    );
}

// ── @for generating multiple messages ────────────────────────────────────────

#[test]
fn for_loop_generates_multiple_messages() {
    let src = concat!(
        "---\nroles:\n  - system\n  - user\n---\n",
        "@for role in roles:\n",
        "@message {role}:\nContent for {role}.\n@end\n",
        "@end\n",
    );
    let result = compile_messages_str(src).expect("should compile");
    assert_eq!(result.messages.len(), 2, "got: {:#?}", result.messages);
    assert_eq!(result.messages[0].role, "system");
    assert_eq!(result.messages[1].role, "user");
}

// ── @if conditionally emits messages ────────────────────────────────────────

#[test]
fn if_block_around_message() {
    let src = concat!(
        "---\ninclude_system: true\n---\n",
        "@if include_system:\n",
        "@message system:\nSystem message.\n@end\n",
        "@end\n",
        "@message user:\nUser message.\n@end\n",
    );
    let result = compile_messages_str(src).expect("should compile");
    assert_eq!(result.messages.len(), 2);
    assert_eq!(result.messages[0].role, "system");
    assert_eq!(result.messages[1].role, "user");
}

#[test]
fn if_block_false_skips_message() {
    let src = concat!(
        "---\ninclude_system: false\n---\n",
        "@if include_system:\n",
        "@message system:\nSystem message.\n@end\n",
        "@end\n",
        "@message user:\nUser message.\n@end\n",
    );
    let result = compile_messages_str(src).expect("should compile");
    assert_eq!(result.messages.len(), 1, "got: {:#?}", result.messages);
    assert_eq!(result.messages[0].role, "user");
}

// ── Whitespace handling ───────────────────────────────────────────────────────

#[test]
fn message_content_is_trimmed() {
    // The body is stripped of leading/trailing whitespace.
    let src = "@message system:\n\n  Hello there.  \n\n@end\n";
    let result = compile_messages_str(src).expect("should compile");
    assert_eq!(
        result.messages[0].content, "Hello there.",
        "content should be trimmed; got: {:?}",
        result.messages[0].content
    );
}

// ── Parser error: missing colon ───────────────────────────────────────────────

#[test]
fn message_without_colon_is_parse_error() {
    // `@message system` (no colon) must produce a parse error.
    let err = compile_messages_str("@message system\nBody.\n@end\n")
        .expect_err("missing colon must be a parse error");
    let msg = err.to_string();
    assert!(
        msg.contains("@message") || msg.contains("colon") || msg.contains("syntax"),
        "got: {msg}"
    );
}
