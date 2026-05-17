mod common;
use common::fixture;

#[test]
fn import_alias() {
    let result = mds::compile(fixture("import_alias.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("Goodbye Alice!"));
}

#[test]
fn import_merge() {
    let result = mds::compile(fixture("import_merge.mds"), None).unwrap();
    assert!(result.contains("Hello Bob!"));
    assert!(result.contains("Goodbye Bob!"));
}

#[test]
fn import_selective() {
    let result = mds::compile(fixture("import_selective.mds"), None).unwrap();
    assert!(result.contains("Hello Charlie!"));
}

#[test]
fn include_directive() {
    let result = mds::compile(fixture("include_test.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("Thank you for using our service."));
}

#[test]
fn reexport() {
    let result = mds::compile(fixture("reexport_consumer.mds"), None).unwrap();
    assert!(result.contains("Hello Dave!"));
}

#[test]
fn wildcard_reexport_barrel() {
    let result = mds::compile(fixture("barrel_consumer.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("- search"));
    assert!(result.contains("- code"));
    assert!(result.contains("- browse"));
}

#[test]
fn circular_import_error() {
    let result = mds::compile(fixture("circular_a.mds"), None);
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
fn absolute_import_path_rejected() {
    let result = mds::compile(fixture("absolute_import.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("relative") || err.contains("import"));
}

#[test]
fn name_collision_on_merge_import() {
    let result = mds::compile(fixture("collision_consumer.mds"), None);
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
    let result = mds::compile(fixture("var_collision_consumer.mds"), None);
    assert!(
        result.is_ok(),
        "merge import should not leak variables (no collision expected), got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn selective_import_prompt_body() {
    let result = mds::compile(fixture("prompt_consumer.mds"), None).unwrap();
    assert!(
        result.contains("This is the module body text."),
        "selective import of 'prompt' should bring in the module's body text, got: {result}"
    );
}

#[test]
fn cross_module_function_preserves_lexical_scope() {
    // A function defined in module A that uses an alias import (u -> utils.mds)
    // must resolve that alias from its *definition* site (lexical scope) even when
    // called from module B, which has no knowledge of 'u'.
    let result = mds::compile(fixture("lexical_scope_consumer.mds"), None).unwrap();
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
fn cross_module_frontmatter_var_in_function() {
    // A function defined in module A that references module A's frontmatter variable
    // must resolve that variable from its *definition* site (lexical scope) even when
    // called from module B, which has no knowledge of that variable.
    let result = mds::compile(fixture("fm_var_consumer.mds"), None).unwrap();
    assert!(
        result.contains("Hello from module A"),
        "expected frontmatter variable to be accessible in cross-module function call, got: {result}"
    );
}

#[test]
fn export_nonexistent_symbol_errors() {
    // @export phantom where 'phantom' is never defined should be a compile error.
    let result = mds::compile(fixture("export_phantom.mds"), None);
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
fn alias_import_no_unqualified_access() {
    // 'greet' was imported under alias 'g', so bare {greet(name)} must fail.
    let result = mds::compile(fixture("alias_no_unqualified.mds"), None);
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

#[test]
fn export_from_no_local_scope() {
    // @export hello from "./greetings.mds" re-exports without local availability.
    let result = mds::compile(fixture("export_from_no_local.mds"), None);
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

#[test]
fn include_respects_export_visibility_for_prompt() {
    // A module with explicit exports that does NOT list "prompt" should not
    // expose its body text via @include, even through an aliased import.
    let dir = tempfile::tempdir().unwrap();
    let provider = dir.path().join("provider.mds");
    let consumer = dir.path().join("consumer.mds");

    std::fs::write(
        &provider,
        "@define greet(name):\nHello {name}!\n@end\n\n@export greet\n\nThis body should be hidden.\n",
    )
    .unwrap();
    std::fs::write(&consumer, "@import \"./provider.mds\" as p\n\n@include p\n").unwrap();

    let (result, warnings) = mds::compile_collecting_warnings(&consumer, None).unwrap();
    // The provider has explicit exports without "prompt", so @include should
    // produce empty output and a warning — not the provider's body text.
    assert!(
        !result.contains("This body should be hidden"),
        "explicit exports without 'prompt' should hide module body from @include, got: {result}"
    );
    assert!(
        warnings.iter().any(|w| w.contains("empty output")),
        "expected warning about empty @include, got warnings: {warnings:?}"
    );
}

#[test]
fn include_empty_body_no_crash() {
    // @include of a module with only function definitions (no body text) should
    // produce an empty string for the include, not crash.
    let result = mds::compile(fixture("include_empty_body.mds"), None).unwrap();
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
fn include_without_import_errors() {
    // @include utils where utils has NOT been @import-ed must fail.
    // Per spec: "Module must be imported first via @import."
    let result = mds::compile(fixture("include_no_import.mds"), None);
    assert!(result.is_err(), "@include of non-imported alias must fail");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("undefined") || err.contains("utils"),
        "error should mention the missing alias, got: {err}"
    );
}

#[test]
fn multilevel_imports() {
    // A imports B (as b), B imports C (as c). A calls b.midfn which calls c.deepfn.
    // Verifies that recursive import resolution works correctly.
    let result = mds::compile(fixture("multilevel_a.mds"), None).unwrap();
    assert!(
        result.contains("Mid via Deep: Alice"),
        "multi-level import should resolve A→B→C, got: {result}"
    );
}

#[test]
fn wildcard_reexport_collision_errors() {
    // Two modules both export 'shared'. Barrel tries @export * from each.
    // Per spec: name collisions across wildcard re-exports → compilation error.
    let result = mds::compile(fixture("wildcard_collision_barrel.mds"), None);
    assert!(
        result.is_err(),
        "wildcard re-export collision should be a compile error"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("collision") || err.contains("shared") || err.contains("already defined"),
        "error should mention the colliding name, got: {err}"
    );
}

#[test]
fn export_prompt_selective_import() {
    // A module with body text uses @export prompt explicitly;
    // another module imports it via selective import and renders the body.
    let result = mds::compile(fixture("export_prompt_consumer.mds"), None).unwrap();
    assert!(
        result.contains("compiler design"),
        "selective import of explicitly-exported 'prompt' should render provider body, got: {result}"
    );
}

#[test]
fn explicit_export_hides_non_exported() {
    // When a module has any @export directive, only exported symbols are visible.
    // collision_a.mds exports 'greet'. A non-exported function should NOT be importable.
    let dir = tempfile::tempdir().unwrap();
    let provider = dir.path().join("provider.mds");
    let consumer = dir.path().join("consumer.mds");

    std::fs::write(
        &provider,
        "@define public_fn(name):\nPublic: {name}\n@end\n\n@define private_fn(name):\nPrivate: {name}\n@end\n\n@export public_fn\n",
    )
    .unwrap();
    std::fs::write(
        &consumer,
        "@import { private_fn } from \"./provider.mds\"\n\n{private_fn(\"Alice\")}\n",
    )
    .unwrap();

    let result = mds::compile(&consumer, None);
    assert!(
        result.is_err(),
        "importing non-exported symbol should fail when module has explicit exports"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("private_fn") || err.contains("not exported"),
        "error should mention the non-exported symbol, got: {err}"
    );
}

#[test]
fn default_public_when_no_exports() {
    // If no @export directives exist, everything is exported (default-public).
    let dir = tempfile::tempdir().unwrap();
    let provider = dir.path().join("provider.mds");
    let consumer = dir.path().join("consumer.mds");

    std::fs::write(&provider, "@define hello(name):\nHello {name}!\n@end\n").unwrap();
    // No @export directive — hello should still be importable.
    std::fs::write(
        &consumer,
        "@import { hello } from \"./provider.mds\"\n\n{hello(\"World\")}\n",
    )
    .unwrap();

    let result = mds::compile(&consumer, None).unwrap();
    assert!(
        result.contains("Hello World!"),
        "default-public module should allow importing any symbol, got: {result}"
    );
}

#[test]
fn selective_import_nonexistent_errors() {
    let result = mds::compile(fixture("selective_import_nonexistent.mds"), None);
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
fn import_nonexistent_file_error() {
    let dir = tempfile::tempdir().unwrap();
    let consumer = dir.path().join("consumer.mds");

    std::fs::write(
        &consumer,
        "@import { greet } from \"./does_not_exist.mds\"\n{greet(\"Alice\")}\n",
    )
    .unwrap();

    let result = mds::compile(&consumer, None);
    assert!(
        result.is_err(),
        "importing a non-existent file should return an error"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("not found") || err.contains("does_not_exist") || err.contains("No such"),
        "error should mention the missing file, got: {err}"
    );
}

#[test]
fn selective_import_of_non_exported_name() {
    // Module has explicit @export, so only exported names are visible.
    let dir = tempfile::tempdir().unwrap();
    let provider = dir.path().join("provider.mds");
    let consumer = dir.path().join("consumer.mds");

    std::fs::write(
        &provider,
        "@define exported(name):\nExported: {name}\n@end\n\
         @define hidden(name):\nHidden: {name}\n@end\n\
         @export exported\n",
    )
    .unwrap();
    std::fs::write(
        &consumer,
        "@import { hidden } from \"./provider.mds\"\n{hidden(\"Alice\")}\n",
    )
    .unwrap();

    let result = mds::compile(&consumer, None);
    assert!(
        result.is_err(),
        "selective import of a non-exported name should fail"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("hidden") || err.contains("not exported") || err.contains("export"),
        "error should mention the non-exported symbol, got: {err}"
    );
}

#[test]
fn reexport_nonexistent_symbol_errors() {
    // @export nonexistent_fn from "./greetings.mds" where greetings.mds does not export
    // 'nonexistent_fn' must produce a clear error at the re-export site.
    let result = mds::compile(fixture("reexport_nonexistent.mds"), None);
    assert!(
        result.is_err(),
        "re-exporting a symbol not found in source module should fail"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("nonexistent_fn") || err.contains("not exported") || err.contains("re-export"),
        "error should mention the missing symbol, got: {err}"
    );
}
