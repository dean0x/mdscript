//! Rule: `insecure-data-origin`
//!
//! **Severity**: Warn (default) | **Tier**: C (never auto-fixed)
//!
//! A URL data entry (spec §4.12) requests an `http://` URL. Only a request map can name
//! the http scheme. The http scheme does not encrypt the request or the response, so
//! each network between the host and the server can read and change them.
//!
//! The rule reads the declarations only and never sends a request. It applies to
//! partials and `@extends` templates too, because their declarations are sent as
//! written.

use crate::ast::Module;
use crate::error::SerializedSpan;
use crate::lint::config::LintConfig;
use crate::lint::diagnostic::{LintDiagnostic, LintResultBuilder, Severity};
use crate::lint::facts::AnalysisContext;

pub(crate) const RULE: &str = "insecure-data-origin";

/// Check the URL data entries for an `http://` URL.
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
    for request in ctx
        .data_requests
        .iter()
        .filter(|request| request.plain_http)
    {
        let pushed = builder.push(LintDiagnostic {
            rule: RULE.to_string(),
            severity,
            message: format!("Data entry '{}' requests an http URL.", request.name),
            help: Some(
                "Use an https URL. The http scheme does not encrypt the request or the response."
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

    fn with_data(data: &str) -> String {
        format!("---\ndata:\n  {data}\n---\n{{{{feed}}}}\n")
    }

    /// TP-100: a request map with an http URL warns, in the flow form and in the block
    /// form, with its span on the entry name.
    #[test]
    fn a_request_map_with_an_http_url_warns() {
        for data in [
            "feed: {url: 'http://a.example/x'}",
            "feed:\n    url: http://a.example/x\n    method: HEAD\n    envelope: true",
        ] {
            let src = with_data(data);
            let diagnostics = lint_src(&src);
            let [diagnostic] = diagnostics.as_slice() else {
                panic!("{data}: expected one diagnostic, got {diagnostics:?}");
            };
            assert_eq!(diagnostic.rule, RULE);
            assert_eq!(diagnostic.severity, Severity::Warn);
            assert_eq!(
                diagnostic.message,
                "Data entry 'feed' requests an http URL."
            );
            let span = diagnostic.span.as_ref().expect("a span on the entry");
            assert_eq!(&src[span.offset..span.offset + span.length], "feed");
        }
    }

    /// Control: an https URL in each form and a data file give no diagnostic.
    #[test]
    fn an_https_url_gives_no_diagnostic() {
        for (data, requests) in [
            ("feed: https://a.example/x", 1),
            ("feed: {url: 'https://a.example/x'}", 1),
            ("feed: ./http.json", 0),
        ] {
            let src = with_data(data);
            assert!(lint_src(&src).is_empty(), "{data}");
            // The declaration parses, so the empty result is not from a refused entry.
            let tokens = tokenize(&src, "test.mds").unwrap();
            let module = parse_with_ctx(&tokens, "test.mds", &src).unwrap();
            let ctx = collect_facts(&module, false, &src).unwrap();
            assert_eq!(ctx.data_requests.len(), requests, "{data}");
            assert_eq!(ctx.frontmatter_vars.len(), 1, "{data}");
        }
    }

    /// The rule can be turned off.
    #[test]
    fn the_rule_can_be_turned_off() {
        let src = with_data("feed: {url: 'http://a.example/x'}");
        let tokens = tokenize(&src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "test.mds", &src).unwrap();
        let ctx = collect_facts(&module, false, &src).unwrap();
        let config = LintConfig::from_rules_checked([(RULE.to_string(), Severity::Off)].into()).0;
        let mut builder = LintResultBuilder::new();
        check(&module, &ctx, "test.mds", &config, &mut builder);
        assert!(builder.build(false).diagnostics.is_empty());
    }
}
