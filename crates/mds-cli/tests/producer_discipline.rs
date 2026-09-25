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
//! warnings. Only the first can receive a hostile character, and since #265 only from a
//! custom `FileSystem` backend: the built-in backends refuse a forbidden path character
//! in every import string, entry key and canonical path, so no module key they resolve
//! can carry one, while a `with_fs` backend that rewrites keys is not bound by that
//! refusal. The parser admits an `@include` alias only if it matches
//! `[A-Za-z_][A-Za-z0-9_]*`, so a test of the other two would assert on an input the
//! parser rejects — vacuous, which is the PF-013 failure mode — and none is written.
//! That asymmetry is stated in `print_discipline.rs`'s module doc rather than papered
//! over.
//!
//! # PF-013 evidence
//!
//! - **Reachable vector:** a custom backend maps the clean import `./big.mds` to a
//!   hostile key; nothing between the backend and the warning rejects it.
//!   [`hostile_key_from_a_custom_backend_reaches_the_warning_escaped`] proves the hostile
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
//! - **The built-in route is closed:** [`hostile_import_is_refused_before_the_producer`]
//!   drives the same hostile name through the built-in virtual backend and asserts it is
//!   refused (`mds::import`) before any module — and so any warning — exists.

use std::collections::HashMap;

use mds::{FileSystem, MdsError, Value, VirtualFs};

/// A filename that carries two members of the escape class: ESC (U+001B, the CSI
/// introducer of an ANSI escape sequence) and U+202E RIGHT-TO-LEFT OVERRIDE (Trojan
/// Source, CVE-2021-42574). Written with `\u{…}` escapes so this file holds no raw
/// control bytes.
const HOSTILE_MODULE: &str = "big\u{1b}[31m\u{202e}.mds";

/// The WIRE form the two hostile characters must arrive in.
const ESCAPED_ESC: &str = "\\u001B";
const ESCAPED_RLO: &str = "\\u202E";

/// A module whose evaluation exceeds `MAX_SOURCEMAP_SEGMENTS` (1 000 000): 100 000
/// iterations x 11 segment-producing nodes (6 Text + 5 Interpolation) = 1 100 000
/// segments, which trips the cap inside the *imported* module and takes the
/// `resolver.rs` branch that names the module in its warning.
const BIG_MODULE: &str =
    "@for item in items:\nA{{item}}B{{item}}C{{item}}D{{item}}E{{item}}F\n@end\n";

fn big_vars() -> HashMap<String, Value> {
    let items: Vec<Value> = (0..100_000)
        .map(|_| Value::String("x".to_string()))
        .collect();
    HashMap::from([("items".to_string(), Value::Array(items))])
}

/// The imported-module segment-cap warning among `warnings`, failing closed when it is
/// absent (non-vacuity: every later assertion would otherwise hold over nothing).
fn cap_warning(warnings: &[String]) -> &str {
    warnings
        .iter()
        .find(|w| w.contains("segment cap") && w.contains("imported module"))
        .unwrap_or_else(|| {
            panic!(
                "non-vacuity: the imported-module segment-cap warning must be emitted; \
                 got warnings: {warnings:?}"
            )
        })
}

/// A backend with none of the built-in key checks that maps the clean import
/// `./big.mds` to [`HOSTILE_MODULE`] — the residual #265 leaves to custom backends.
struct KeyRewritingFs(VirtualFs);

impl FileSystem for KeyRewritingFs {
    fn resolve_entry(&self, path: &str) -> Result<String, MdsError> {
        Ok(path.to_string())
    }
    fn normalize_in_dir(&self, dir: &str, relative: &str) -> Result<String, MdsError> {
        match relative {
            "./big.mds" => Ok(HOSTILE_MODULE.to_string()),
            _ => self.0.normalize_in_dir(dir, relative),
        }
    }
    fn parent_dir(&self, key: &str) -> String {
        self.0.parent_dir(key)
    }
    fn read(&self, normalized: &str) -> Result<String, MdsError> {
        self.0.read(normalized)
    }
    fn is_markdown(&self, normalized: &str) -> bool {
        self.0.is_markdown(normalized)
    }
}

/// AC / #176: the imported-module filename in `mds-core`'s source-map segment-cap warning
/// is WIRE-escaped at construction, so the string `mds-cli` hands to `eprint_warning` —
/// which preserves `\n` — cannot carry a terminal-hazardous byte.
#[test]
fn hostile_key_from_a_custom_backend_reaches_the_warning_escaped() {
    let modules = HashMap::from([
        (
            "entry.mds".to_string(),
            "@import \"./big.mds\" as big\n@include big\n".to_string(),
        ),
        (HOSTILE_MODULE.to_string(), BIG_MODULE.to_string()),
    ]);
    let mut cache = mds::ModuleCache::with_fs(Box::new(KeyRewritingFs(VirtualFs::new(modules))));
    let mut warnings = vec![];
    cache
        .resolve_virtual_intrinsic_opts(
            "entry.mds",
            &big_vars(),
            &mds::CompileOptions::default().with_source_map(true),
            &mut warnings,
        )
        .expect("compilation must succeed even when the segment cap is hit");
    let cap_warning = cap_warning(&warnings);

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

/// #265: on the built-in backend the same hostile name cannot reach the producer — the
/// import string is refused (`mds::import`) before the module is resolved. Control: the
/// same compile under a clean name does emit the warning, naming the module.
#[test]
fn hostile_import_is_refused_before_the_producer() {
    let compile = |name: &str| {
        let modules = HashMap::from([
            (
                "entry.mds".to_string(),
                format!("@import \"./{name}\" as big\n@include big\n"),
            ),
            (name.to_string(), BIG_MODULE.to_string()),
        ]);
        mds::compile_virtual_with_deps_opts(
            modules,
            "entry.mds",
            Some(big_vars()),
            mds::CompileOptions::default().with_source_map(true),
        )
    };

    let err = compile(HOSTILE_MODULE).expect_err("the hostile import must be refused");
    let code = miette::Diagnostic::code(&err).map(|c| c.to_string());
    assert_eq!(code.as_deref(), Some("mds::import"), "{err:?}");
    let msg = err.to_string();
    assert!(
        msg.contains("import path contains forbidden character U+001B")
            && msg.contains(ESCAPED_ESC)
            && msg.contains(ESCAPED_RLO),
        "the refusal names the codepoint and shows the name escaped: {msg:?}"
    );
    assert!(
        !msg.contains('\u{1b}') && !msg.contains('\u{202e}'),
        "no raw hostile char in the refusal: {msg:?}"
    );

    let result = compile("big.mds").expect("control: a clean name compiles");
    assert!(
        cap_warning(&result.warnings).contains("big.mds"),
        "control: the warning names the module"
    );
}
