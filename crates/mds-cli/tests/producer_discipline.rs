//! Producer-discipline test — the one cross-crate precondition
//! `crates/mds-cli/tests/print_discipline.rs` depends on and cannot check
//! (CWE-117 / PF-013 / #176).
//!
//! # Why this lives here, and why it is narrow
//!
//! `build.rs` and `main.rs` print whole `mds-core` warning strings with
//! `for w in &result.warnings { eprint_warning(w) }`. `eprint_warning` escapes in HUMAN
//! mode, which preserves `\n` by design so multi-line prose renders — so what stops a
//! warning from forging a bare `Clean: …` status line is **not** the print helper. It is
//! that `mds-core` WIRE-escapes every untrusted value it interpolates at construction.
//! `print_discipline.rs` is a lexical scanner over `crates/mds-cli/src/**`; it cannot
//! follow a value across the crate boundary, so those two sites sit in its
//! `ALLOWED_UNTRACED_HELPER_ARGS` with that dependency written down.
//!
//! `mds-core` has exactly three warning producers that interpolate a runtime value —
//! `resolver.rs`'s imported-module filename and `evaluator.rs`'s two `@include` alias
//! warnings. Only the first can receive a hostile character: a module key is a filesystem
//! path, and POSIX permits any byte but `/` and NUL in a filename. The parser admits an
//! `@include` alias only if it matches `[A-Za-z_][A-Za-z0-9_]*`, so a test of the other
//! two would assert on an input the parser rejects — vacuous, which is the PF-013 failure
//! mode — and none is written. That asymmetry is stated in `print_discipline.rs`'s module
//! doc rather than papered over.
//!
//! # PF-013 evidence
//!
//! - **Reachable vector:** the module map handed to `compile_virtual_with_deps_opts` is
//!   keyed by path, and no layer between the caller and the warning rejects a control
//!   byte in that key. [`hostile_module_name_reaches_the_warning`] proves the hostile
//!   name really does reach this producer by finding it, escaped, in the warning text.
//! - **Positive:** the escaped `\u001B` literal must be present in the warning.
//! - **Negative:** no raw ESC byte, and no raw `\n`, may appear anywhere in it.
//! - **Non-vacuity:** the warning must exist and must be the segment-cap warning. Without
//!   that, a compile that silently stopped emitting the warning would pass every
//!   assertion above by producing nothing to assert on.
//! - **Guard-removal:** deleting the `sanitize_control_chars_wire(…)` call at
//!   `crates/mds-core/src/resolver.rs`'s segment-cap `warnings.push` makes the positive
//!   assertion fail on the missing `\u001B` literal and the negative assertion fail on
//!   the raw ESC byte.

/// A filename that carries two members of the escape class: ESC (U+001B, the CSI
/// introducer of an ANSI escape sequence) and U+202E RIGHT-TO-LEFT OVERRIDE (Trojan
/// Source, CVE-2021-42574). Written with `\u{…}` escapes so this file holds no raw
/// control bytes.
const HOSTILE_MODULE: &str = "big\u{1b}[31m\u{202e}.mds";

/// The WIRE form the two hostile characters must arrive in.
const ESCAPED_ESC: &str = "\\u001B";
const ESCAPED_RLO: &str = "\\u202E";

/// AC / #176: the imported-module filename in `mds-core`'s source-map segment-cap warning
/// is WIRE-escaped at construction, so the string `mds-cli` hands to `eprint_warning` —
/// which preserves `\n` — cannot carry a terminal-hazardous byte.
///
/// The vector: a module whose evaluation exceeds `MAX_SOURCEMAP_SEGMENTS` (1 000 000),
/// imported by an entry module. 100 000 iterations x 11 segment-producing nodes =
/// 1 100 000 segments, which trips the cap inside the *imported* module and takes the
/// `resolver.rs` branch that names the module in its warning.
#[test]
fn hostile_module_name_reaches_the_warning() {
    use mds::{CompileOptions, Value};

    let items: Vec<Value> = (0..100_000)
        .map(|_| Value::String("x".to_string()))
        .collect();
    let mut vars = std::collections::HashMap::new();
    vars.insert("items".to_string(), Value::Array(items));

    let mut modules = std::collections::HashMap::new();
    // The resolver requires an explicitly relative specifier; it normalizes back to the
    // bare module key, which is what `ctx.file_str` — and therefore the warning — carries.
    modules.insert(
        "entry.mds".to_string(),
        format!("@import \"./{HOSTILE_MODULE}\" as big\n@include big\n"),
    );
    // 6 Text nodes + 5 Interpolation nodes = 11 segments per iteration.
    modules.insert(
        HOSTILE_MODULE.to_string(),
        "@for item in items:\nA{{item}}B{{item}}C{{item}}D{{item}}E{{item}}F\n@end\n".to_string(),
    );

    let result = mds::compile_virtual_with_deps_opts(
        modules,
        "entry.mds",
        Some(vars),
        CompileOptions {
            source_map: true,
            ..Default::default()
        },
    )
    .expect("compilation must succeed even when the segment cap is hit");

    // Non-vacuity: the producer under test must actually have run. If the cap warning
    // stops being emitted, every assertion below would hold over an empty haystack.
    let cap_warning = result
        .warnings
        .iter()
        .find(|w| w.contains("segment cap") && w.contains("imported module"))
        .unwrap_or_else(|| {
            panic!(
                "non-vacuity: the imported-module segment-cap warning must be emitted; \
                 got warnings: {:?}",
                result.warnings
            )
        });

    // Reachability + positive: the hostile name reached the producer, and both hostile
    // characters arrived as their six-character WIRE literals.
    assert!(
        cap_warning.contains(ESCAPED_ESC),
        "the ESC byte in the module name must arrive as the literal {ESCAPED_ESC}; \
         got {cap_warning:?}"
    );
    assert!(
        cap_warning.contains(ESCAPED_RLO),
        "U+202E in the module name must arrive as the literal {ESCAPED_RLO}; \
         got {cap_warning:?}"
    );

    // Negative: no member of the escape class survives raw. `\n` is checked explicitly —
    // it is the line-forgery vector (CWE-117) that HUMAN-mode `eprint_warning` would
    // preserve, and the whole reason this producer uses WIRE mode.
    assert!(
        !cap_warning.contains('\u{1b}'),
        "a raw ESC byte must not survive into a warning string: {cap_warning:?}"
    );
    assert!(
        !cap_warning.contains('\u{202e}'),
        "a raw U+202E must not survive into a warning string: {cap_warning:?}"
    );
    assert!(
        !cap_warning.contains('\n'),
        "a raw newline must not survive into a warning string — `eprint_warning` \
         preserves it, so it would forge a standalone status line: {cap_warning:?}"
    );
}
