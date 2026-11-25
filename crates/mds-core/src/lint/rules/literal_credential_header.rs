//! Rule: `literal-credential-header`
//!
//! **Severity**: Warn (default) | **Tier**: C (never auto-fixed)
//!
//! A URL data entry (spec §4.12) sets a header to literal text, and the lower-case
//! header name contains `token`, `secret`, `api-key`, `apikey` or `password`. The value
//! is then part of the template source. The secret form `{secret: NAME, prefix: ''}` lets the host
//! give the value for the origin of the request.
//!
//! A value with a `{{…}}` reference takes its text from a variable, so the rule does not
//! report it. The check gate refuses literal text for `Authorization`,
//! `Proxy-Authorization`, `Cookie` and `X-Api-Key`, which accept the secret form only.
//!
//! The rule reads the declarations only and never sends a request. Its message names
//! the entry and the header, never the value. It applies to partials and `@extends`
//! templates too, because their declarations are sent as written.

use crate::ast::Module;
use crate::error::SerializedSpan;
use crate::lint::config::LintConfig;
use crate::lint::diagnostic::{LintDiagnostic, LintResultBuilder, Severity};
use crate::lint::facts::AnalysisContext;

pub(crate) const RULE: &str = "literal-credential-header";

/// The parts of a lower-case header name that mark a credential.
const CREDENTIAL_WORDS: [&str; 5] = ["token", "secret", "api-key", "apikey", "password"];

/// The header name `name` marks a credential, in any ASCII letter case.
fn names_a_credential(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    CREDENTIAL_WORDS.iter().any(|word| lower.contains(word))
}

/// Check the URL data entries for a credential header with a literal value.
pub(crate) fn check(
    _module: &Module,
    ctx: &AnalysisContext,
    filename: &str,
    config: &LintConfig,
    builder: &mut LintResultBuilder,
) {
    let severity = config.severity_for(RULE).copied().unwrap_or(Severity::Warn);
    if severity == Severity::Off {
        return;
    }
    for request in &ctx.data_requests {
        for header in request
            .literal_headers
            .iter()
            .filter(|header| names_a_credential(header))
        {
            let pushed = builder.push(LintDiagnostic {
                rule: RULE.to_string(),
                severity,
                message: format!(
                    "Data entry '{}' sets the credential header '{header}' to literal text.",
                    request.name
                ),
                help: Some(
                    "Use the secret form `{secret: NAME, prefix: ''}`, so that the host gives the value."
                        .to_string(),
                ),
                span: request.approx_offset.map(|offset| SerializedSpan {
                    offset,
                    length: request.name.len(),
                    line: None,
                    column: None,
                }),
                file: Some(filename.to_string()),
                fix_removals: None,
                fix_edits: None,
            });
            if !pushed {
                return;
            }
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::lint::facts::collect_facts;
    use crate::parser::parse_with_ctx;

    fn lint_src(src: &str) -> Vec<LintDiagnostic> {
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "test.mds", src).unwrap();
        let ctx = collect_facts(&module, false, src).unwrap();
        let mut builder = LintResultBuilder::new();
        check(
            &module,
            &ctx,
            "test.mds",
            &LintConfig::default(),
            &mut builder,
        );
        builder.build(false).diagnostics
    }

    fn with_header(header: &str) -> String {
        format!("---\ndata:\n  api:\n    url: https://a.example/x\n    headers:\n      {header}\n---\n{{{{api}}}}\n")
    }

    /// TP-100: each credential word in a literal header name gives a warning that names
    /// the entry and the header, never the value, with its span on the entry name. The
    /// check for the value over every diagnostic comes first, and the messages are fixed
    /// text or the header name, so a failure can not show the value.
    #[test]
    fn a_literal_credential_header_warns() {
        for name in [
            "X-Auth-Token",
            "Client-Secret",
            "Api-Key",
            "ApiKey",
            "X-PASSWORD",
        ] {
            let src = with_header(&format!("{name}: planted-value"));
            let diagnostics = lint_src(&src);
            assert!(
                !format!("{diagnostics:?}").contains("planted-value"),
                "{name}: no diagnostic may show the header value"
            );
            let [diagnostic] = diagnostics.as_slice() else {
                panic!("{name}: expected one diagnostic, got {}", diagnostics.len());
            };
            assert_eq!(diagnostic.rule, RULE, "{name}: the rule");
            assert_eq!(diagnostic.severity, Severity::Warn, "{name}: the severity");
            assert_eq!(
                diagnostic.message,
                format!("Data entry 'api' sets the credential header '{name}' to literal text."),
                "{name}: the message must name the entry and the header"
            );
            let span = diagnostic.span.as_ref().expect("a span on the entry");
            // The source holds the value, so the span text is compared, not printed.
            assert!(
                src.get(span.offset..span.offset + span.length) == Some("api"),
                "{name}: the span must cover the entry name"
            );
        }
    }

    /// Control: the secret form, a value with a reference and a header name with no
    /// credential word give no diagnostic. A header line can hold the value, so the
    /// messages name its index.
    #[test]
    fn the_secret_form_and_other_headers_give_no_diagnostic() {
        for (index, header) in [
            "X-Auth-Token: {secret: TOKEN, prefix: ''}",
            "X-Auth-Token: {secret: TOKEN, prefix: 'Bearer '}",
            "X-Auth-Token: 'Bearer {{key}}'",
            "X-Trace: planted-value",
        ]
        .into_iter()
        .enumerate()
        {
            let src = with_header(header).replace("---\n{{api}}", "key: k\n---\n{{api}}");
            // The declaration parses, so the empty result is not from a refused entry.
            let tokens = tokenize(&src, "test.mds").unwrap();
            let module = parse_with_ctx(&tokens, "test.mds", &src).unwrap();
            let ctx = collect_facts(&module, false, &src).unwrap();
            assert_eq!(
                ctx.data_requests.len(),
                1,
                "header {index}: the entry must parse"
            );
            assert!(lint_src(&src).is_empty(), "header {index}: no diagnostic");
        }
    }

    /// The severity can be raised or turned off.
    #[test]
    fn the_severity_follows_the_config() {
        let src = with_header("X-Auth-Token: planted-value");
        let tokens = tokenize(&src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "test.mds", &src).unwrap();
        let ctx = collect_facts(&module, false, &src).unwrap();
        for (severity, count) in [(Severity::Error, 1), (Severity::Off, 0)] {
            let config = LintConfig::from_rules_checked([(RULE.to_string(), severity)].into()).0;
            let mut builder = LintResultBuilder::new();
            check(&module, &ctx, "test.mds", &config, &mut builder);
            let diagnostics = builder.build(false).diagnostics;
            assert_eq!(diagnostics.len(), count, "{severity:?}");
            assert!(diagnostics.iter().all(|d| d.severity == severity));
        }
    }
}
