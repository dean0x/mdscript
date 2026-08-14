//! Lint rule implementations.
//!
//! Each rule is a plain function `check(module, ctx, filename, config, builder)` with
//! no generics — non-generic dispatch prevents monomorphization overhead (AC-PERF-02).
//!
//! ## Rule catalogue (built-in severity defaults)
//!
//! | Rule                    | Severity | Tier | Description                                   |
//! |-------------------------|----------|------|-----------------------------------------------|
//! | `empty-block`           | Warn     | A    | Block body is empty or whitespace-only        |
//! | `redundant-else`        | Warn     | C    | @else body structurally identical to then-body|
//! | `unreachable-branch`    | Error    | A    | Literal↔literal always-true/false conditions  |
//! | `duplicate-import`      | Error    | A    | Same import path declared twice               |
//! | `duplicate-export`      | Error    | A    | Same export name declared twice               |
//! | `unused-variable`       | Warn     | C    | FM key never referenced in body               |
//! | `unused-import`         | Warn     | B    | Import not used in body                       |
//! | `unused-function`       | Warn     | B    | @define not exported and not called           |
//! | `shadow-variable`       | Info     | C    | Inner scope name hides outer binding          |
//! | `legacy-interpolation`  | Warn     | A    | Single-brace `{x}` syntax (migrates to `{{x}}`) |

pub(crate) mod duplicate_export;
pub(crate) mod duplicate_import;
pub(crate) mod empty_block;
pub(crate) mod legacy_interpolation;
pub(crate) mod redundant_else;
pub(crate) mod shadow_variable;
pub(crate) mod unreachable_branch;
pub(crate) mod unused_function;
pub(crate) mod unused_import;
pub(crate) mod unused_variable;

pub(crate) mod structural_eq;

/// All registered lint rule names, sorted lexicographically.
///
/// AD-224-2: composed from each rule module's own `RULE` const so the string
/// is the single source of truth. This does NOT close the *omission* risk — a
/// new module whose `RULE` is never listed here compiles fine. The mechanical
/// closure is the bidirectional tier table in `tier.rs` plus an explicit
/// `ALL_RULE_NAMES.len() == 10` pin in that table's test.
pub(crate) const ALL_RULE_NAMES: &[&str] = &[
    duplicate_export::RULE,
    duplicate_import::RULE,
    empty_block::RULE,
    legacy_interpolation::RULE,
    redundant_else::RULE,
    shadow_variable::RULE,
    unreachable_branch::RULE,
    unused_function::RULE,
    unused_import::RULE,
    unused_variable::RULE,
];
