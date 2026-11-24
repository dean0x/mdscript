//! A `@message` role must not come from data. Each test compiles a control
//! fixture that has no data source and then the same template with one.

use std::collections::HashMap;

use mds::{
    compile_virtual, lint_virtual, LintConfig, LintResult, SerializedError, Severity, Value,
};

const SETTINGS: &str = "---\ndata: {settings: ./roles.yml}\n---\n";
const ORDINARY: &str = "---\nsettings: {role: user}\n---\n";
const ROLES: (&str, &str) = ("roles.yml", "role: user");
const MESSAGE: &str = "@message {{settings.role}}:\nBody\n@end\n";

fn files(source: &str, extra: &[(&str, &str)]) -> HashMap<String, String> {
    std::iter::once(("main.mds", source))
        .chain(extra.iter().copied())
        .map(|(path, text)| (path.into(), text.into()))
        .collect()
}

fn roles(
    source: &str,
    extra: &[(&str, &str)],
    vars: Option<HashMap<String, Value>>,
) -> Result<Vec<String>, SerializedError> {
    compile_virtual(files(source, extra), "main.mds", vars)
        .and_then(|result| result.into_messages())
        .map(|messages| messages.into_iter().map(|message| message.role).collect())
        .map_err(|error| error.serialize())
}

fn assert_data_role(result: Result<Vec<String>, SerializedError>, module: &str) {
    let error = result.expect_err("a role from data must not reach output");
    assert_eq!(error.code, "mds::data", "{error:?}");
    assert_eq!(
        error.message,
        format!("data error in \"{module}\": @message role must not depend on a data entry"),
    );
}

#[test]
fn a_data_field_cannot_supply_a_role() {
    let literal = format!("{SETTINGS}@message user:\nBody\n@end\n");
    assert_eq!(roles(&literal, &[ROLES], None).unwrap(), ["user"]);
    assert_eq!(
        roles(&format!("{ORDINARY}{MESSAGE}"), &[], None).unwrap(),
        ["user"]
    );
    assert_data_role(
        roles(&format!("{SETTINGS}{MESSAGE}"), &[ROLES], None),
        "main.mds",
    );
}

#[test]
fn a_replayed_entry_keeps_its_provenance() {
    let body = "@message {{settings}}:\nBody\n@end\n";
    let vars = || Some(HashMap::from([("settings".into(), Value::from("user"))]));
    assert_eq!(roles(body, &[], vars()).unwrap(), ["user"]);
    assert_data_role(roles(&format!("{SETTINGS}{body}"), &[], vars()), "main.mds");
}

#[test]
fn an_array_item_keeps_the_iterables_provenance() {
    let body = "@for item in settings:\n@message {{item}}:\nBody\n@end\n@end\n";
    let control = format!("---\nsettings: [user]\n---\n{body}");
    assert_eq!(roles(&control, &[], None).unwrap(), ["user"]);
    let marked = format!("{SETTINGS}{body}");
    assert_data_role(roles(&marked, &[("roles.yml", "[user]")], None), "main.mds");
}

#[test]
fn an_object_key_keeps_the_iterables_provenance() {
    let body = "@for key, value in settings:\n@message {{key}}:\nBody\n@end\n@end\n";
    let control = format!("---\nsettings: {{user: label}}\n---\n{body}");
    assert_eq!(roles(&control, &[], None).unwrap(), ["user"]);
    let marked = format!("{SETTINGS}{body}");
    assert_data_role(
        roles(&marked, &[("roles.yml", "user: label")], None),
        "main.mds",
    );
}

#[test]
fn an_object_value_keeps_the_iterables_provenance() {
    let body = "@for key, value in settings:\n@message {{value}}:\nBody\n@end\n@end\n";
    let control = format!("---\nsettings: {{label: user}}\n---\n{body}");
    assert_eq!(roles(&control, &[], None).unwrap(), ["user"]);
    let marked = format!("{SETTINGS}{body}");
    assert_data_role(
        roles(&marked, &[("roles.yml", "label: user")], None),
        "main.mds",
    );
}

#[test]
fn a_loop_variable_over_an_ordinary_value_shadows_an_entry() {
    let head = "---\ndata: {settings: ./roles.yml}\nroles: [user]\n---\n";
    let body = "@message {{settings}}:\nBody\n@end\n@end\n";
    let control = format!("{head}@for settings in roles:\n{body}");
    assert_eq!(
        roles(&control, &[("roles.yml", "[user]")], None).unwrap(),
        ["user"]
    );
    let marked = format!("{head}@for settings in settings:\n{body}");
    assert_data_role(roles(&marked, &[("roles.yml", "[user]")], None), "main.mds");
}

#[test]
fn a_builtin_call_keeps_argument_provenance() {
    let message = "@message {{lower(upper(settings.role))}}:\nBody\n@end\n";
    assert_eq!(
        roles(&format!("{ORDINARY}{message}"), &[], None).unwrap(),
        ["user"]
    );
    assert_data_role(
        roles(&format!("{SETTINGS}{message}"), &[ROLES], None),
        "main.mds",
    );
}

#[test]
fn a_function_argument_keeps_its_provenance() {
    let echo = ("echo.mds", "@define echo(text):\n{{text}}\n@end\n");
    let message =
        "@import { echo } from \"./echo.mds\"\n@message {{echo(settings.role)}}:\nBody\n@end\n";
    let control = format!("{ORDINARY}{message}");
    assert_eq!(roles(&control, &[echo], None).unwrap(), ["user"]);
    let marked = format!("{SETTINGS}{message}");
    assert_data_role(roles(&marked, &[echo, ROLES], None), "main.mds");
}

#[test]
fn a_function_from_a_module_with_data_cannot_supply_a_role() {
    let main = "@import \"./roles.mds\" as roles\n@message {{roles.make()}}:\nBody\n@end\n";
    let make = "@define make():\n{{settings.role}}\n@end\n";
    let control = format!("{ORDINARY}{make}");
    assert_eq!(
        roles(main, &[("roles.mds", &control)], None).unwrap(),
        ["user"]
    );
    let marked = format!("{SETTINGS}{make}");
    assert_data_role(
        roles(main, &[("roles.mds", &marked), ROLES], None),
        "main.mds",
    );
}

#[test]
fn a_function_that_calls_a_module_with_data_cannot_supply_a_role() {
    let main = "@import \"./outer.mds\" as outer\n@message {{outer.role()}}:\nBody\n@end\n";
    let outer = (
        "outer.mds",
        "@import { make } from \"./inner.mds\"\n@define role():\n{{make()}}\n@end\n",
    );
    let make = "@define make():\n{{settings.role}}\n@end\n";
    let control = format!("{ORDINARY}{make}");
    assert_eq!(
        roles(main, &[outer, ("inner.mds", &control)], None).unwrap(),
        ["user"]
    );
    let marked = format!("{SETTINGS}{make}");
    assert_data_role(
        roles(main, &[outer, ("inner.mds", &marked), ROLES], None),
        "main.mds",
    );
}

#[test]
fn an_imported_prompt_from_a_module_with_data_cannot_supply_a_role() {
    let main = "@import { prompt } from \"./label.mds\"\n@message {{prompt}}:\nBody\n@end\n";
    let label = "{{settings.role}}\n";
    let control = format!("{ORDINARY}{label}");
    assert_eq!(
        roles(main, &[("label.mds", &control)], None).unwrap(),
        ["user\n"]
    );
    let marked = format!("{SETTINGS}{label}");
    assert_data_role(
        roles(main, &[("label.mds", &marked), ROLES], None),
        "main.mds",
    );
}

#[test]
fn an_extends_base_function_over_data_cannot_supply_a_role() {
    let child = "@extends \"./base.mds\"\n@block extra:\n@end\n";
    let base = "@define make():\n{{settings.role}}\n@end\n@message {{make()}}:\nBody\n@end\n@block extra:\n@end\n";
    let control = format!("{ORDINARY}{base}");
    assert_eq!(
        roles(child, &[("base.mds", &control)], None).unwrap(),
        ["user"]
    );
    let marked = format!("{SETTINGS}{base}");
    assert_data_role(
        roles(child, &[("base.mds", &marked), ROLES], None),
        "base.mds",
    );
}

fn lint(source: &str, entry: &str, extra: &[(&str, &str)], config: &LintConfig) -> LintResult {
    let mut modules = files(source, extra);
    if entry != "main.mds" {
        let text = modules.remove("main.mds").unwrap();
        modules.insert(entry.into(), text);
    }
    lint_virtual(modules, entry, None, config).unwrap()
}

fn messages(result: LintResult, rule: &str) -> Vec<String> {
    result
        .diagnostics
        .into_iter()
        .filter(|diagnostic| diagnostic.rule == rule)
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn lint_reports_a_loop_variable_that_shadows_a_data_entry() {
    let mut config = LintConfig::default();
    config
        .rules
        .insert("shadow-variable".into(), Severity::Warn);
    let head = "---\ndata: {settings: ./roles.yml}\nroles: [user]\n---\n";
    let control = format!("{head}@for item in roles:\n{{{{item}}}}\n@end\n{{{{settings}}}}\n");
    assert!(messages(lint(&control, "main.mds", &[], &config), "shadow-variable").is_empty());
    let marked = format!("{head}@for settings in roles:\n{{{{settings}}}}\n@end\n");
    assert_eq!(
        messages(lint(&marked, "main.mds", &[], &config), "shadow-variable"),
        ["Variable 'settings' (@for loop variable) shadows an outer data entry with the same name."]
    );
}

#[test]
fn lint_reports_an_unused_data_entry_with_partial_and_extends_exemptions() {
    let config = LintConfig::default();
    let unused = format!("{SETTINGS}Body\n");
    assert_eq!(
        messages(lint(&unused, "main.mds", &[], &config), "unused-variable"),
        ["Data entry 'settings' is declared in frontmatter but never referenced in the body."]
    );
    let used = format!("{SETTINGS}{{{{settings.role}}}}\n");
    assert!(messages(lint(&used, "main.mds", &[], &config), "unused-variable").is_empty());
    assert!(messages(lint(&unused, "_main.mds", &[], &config), "unused-variable").is_empty());
    let child = format!("{SETTINGS}@extends \"./base.mds\"\n");
    let base = ("base.mds", "@block body:\nBody\n@end\n");
    assert!(messages(
        lint(&child, "main.mds", &[base], &config),
        "unused-variable"
    )
    .is_empty());
}

#[test]
fn lint_counts_a_data_reference_as_a_use() {
    let config = LintConfig::default();
    let head = |url: &str| {
        format!("---\ndata:\n  base: ./base.yml\n  api: '{url}'\n---\n{{{{api.id}}}}\n")
    };
    let control = head("https://example.com/items");
    assert_eq!(
        messages(lint(&control, "main.mds", &[], &config), "unused-variable"),
        ["Data entry 'base' is declared in frontmatter but never referenced in the body."]
    );
    let used = head("https://example.com/{{base.id}}");
    assert!(messages(lint(&used, "main.mds", &[], &config), "unused-variable").is_empty());
}
