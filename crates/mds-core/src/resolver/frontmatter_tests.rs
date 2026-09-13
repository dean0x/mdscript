//! Bounds tests for frontmatter YAML parsing (#162).
//!
//! Covers the 1 MiB size cap, the node budget (the alias-fan-out bound), the depth
//! pins that `serde_yaml_ng` and `Value::from_yaml` already enforce, and byte-for-byte
//! parity between the budgeted `parse_frontmatter_yaml` and the unbounded
//! `serde_yaml_ng::from_str` for every input that is not an amplification attack.
//!
//! Wired into `frontmatter.rs` as `#[cfg(test)] #[path = "frontmatter_tests.rs"] mod
//! frontmatter_tests;`, so the private helpers (`parse_frontmatter_yaml_bounded`) are in
//! scope via `super::`.

use super::{parse_frontmatter_yaml, parse_frontmatter_yaml_bounded};
use crate::error::MdsError;
use crate::limits::{MAX_FRONTMATTER_NODES, MAX_FRONTMATTER_SIZE};

// ── Predicates ────────────────────────────────────────────────────────────────

fn is_rl<T>(r: &Result<T, MdsError>) -> bool {
    matches!(r, Err(MdsError::ResourceLimit { .. }))
}

fn is_yaml<T>(r: &Result<T, MdsError>) -> bool {
    matches!(r, Err(MdsError::YamlError { .. }))
}

fn msg<T: std::fmt::Debug>(r: &Result<T, MdsError>) -> String {
    match r {
        Err(e) => e.to_string(),
        Ok(v) => panic!("expected Err, got Ok({v:?})"),
    }
}

// ── Builders ────────────────────────────────────────────────────────────────

/// Wrap a YAML frontmatter body in `---` fences with a one-line body. `\n` only, since
/// `fm.raw` is CR-stripped before it reaches the parser.
fn wrap(yaml: &str) -> String {
    format!("---\n{yaml}---\nHi\n")
}

/// A single `k: <sentinel><padding>\n` line whose total byte length is EXACTLY `bytes`.
///
/// The `ZZSENTINELZZ` marker lets the size-cap tests assert the rejection message never
/// echoes the (arbitrarily large) frontmatter content back to the user.
fn fm_of_size(bytes: usize) -> String {
    const PREFIX: &str = "k: ZZSENTINELZZ";
    assert!(bytes > PREFIX.len() + 1, "requested size too small");
    // PREFIX + pad + '\n' == bytes.
    let pad = bytes - PREFIX.len() - 1;
    let out = format!("{PREFIX}{}\n", "x".repeat(pad));
    assert_eq!(
        out.len(),
        bytes,
        "fm_of_size must produce EXACTLY `bytes` bytes"
    );
    out
}

/// Node count charged by the budgeted deserialiser for `alias_bomb(n, m, pad)`:
/// - top mapping container: 1
/// - key `a` (1) + its anchored sequence of `n` scalars (1 + n)
/// - key `b` (1) + a sequence (1) of `m` aliases, each expanding to `1 + n` nodes
/// - when `pad > 0`: key `c` (1) + a sequence (1) of `pad` scalars (pad)
fn bomb_nodes(n: usize, m: usize, pad: usize) -> usize {
    let core = 1 + 2 + n + 2 + m * (1 + n);
    if pad > 0 {
        core + 2 + pad
    } else {
        core
    }
}

/// An alias-fan-out bomb: `a: &a [x, x, ...(n)]`, `b: [*a, *a, ...(m)]`, and — when
/// `pad > 0` — `c: [x, x, ...(pad)]`. The materialised tree has `bomb_nodes(n, m, pad)`
/// nodes because each `*a` expands to the full `n`-element sequence at deserialise time.
fn alias_bomb(n: usize, m: usize, pad: usize) -> String {
    let xs = vec!["x"; n].join(", ");
    let refs = vec!["*a"; m].join(", ");
    let mut out = format!("a: &a [{xs}]\nb: [{refs}]\n");
    if pad > 0 {
        let ps = vec!["x"; pad].join(", ");
        out.push_str(&format!("c: [{ps}]\n"));
    }
    out
}

/// Build an alias bomb whose materialised node count is EXACTLY `target` (using n = 1000
/// and always emitting the `c` padding sequence so `pad >= 1`). Asserts the count.
fn bomb_with_nodes(target: usize) -> String {
    const N: usize = 1000;
    // total = bomb_nodes(N, m, pad) = 1007 + 1001*m + pad   (with pad > 0)
    assert!(target > 1007 + 1001, "target too small to solve");
    let budget = target - 1007;
    let mut m = budget / 1001;
    let mut pad = budget - m * 1001;
    if pad == 0 {
        // Keep the `c` sequence non-empty so the (2 + pad) accounting term applies.
        m -= 1;
        pad += 1001;
    }
    let out = alias_bomb(N, m, pad);
    assert_eq!(
        bomb_nodes(N, m, pad),
        target,
        "bomb_with_nodes solved to the wrong count"
    );
    out
}

/// `k: [[[...x...]]]` with `d` nested flow sequences around a scalar.
fn nested_flow_seq(d: usize) -> String {
    format!("k: {}x{}\n", "[".repeat(d), "]".repeat(d))
}

/// `d`-deep nested block mappings: `a:\n  a:\n    a: ...`.
fn nested_block_map(d: usize) -> String {
    let mut out = String::new();
    for i in 0..d {
        out.push_str(&"  ".repeat(i));
        out.push_str("a:\n");
    }
    out.push_str(&"  ".repeat(d));
    out.push_str("v\n");
    out
}

/// `d` nested tagged flow sequences around a scalar: `!t [!t [ ... 5 ... ]]`. Each level
/// is a tag on a one-element sequence (a node may carry only one tag, so tags cannot be
/// stacked directly). Every level adds a `Tagged` + a `Sequence` to the value tree.
fn tagged_nest(d: usize) -> String {
    let mut inner = String::from("5");
    for _ in 0..d {
        inner = format!("!t [{inner}]");
    }
    format!("k: {inner}\n")
}

/// Billion-laughs style multi-level alias amplification with `levels` anchor levels each
/// referencing the previous one `fanout` times. `serde_yaml_ng`'s own repetition limit
/// (alias-jump count) is what stops this, not our node budget.
fn laughs(fanout: usize, levels: usize) -> String {
    let mut out = String::new();
    // Level 0: a literal list of scalars.
    let leaves = vec!["\"x\""; fanout].join(", ");
    out.push_str(&format!("l0: &l0 [{leaves}]\n"));
    for i in 1..levels {
        let refs = vec![format!("*l{}", i - 1); fanout].join(", ");
        out.push_str(&format!("l{i}: &l{i} [{refs}]\n"));
    }
    out
}

// ── T0: constant pins ─────────────────────────────────────────────────────────

#[test]
fn t0_constants() {
    assert_eq!(MAX_FRONTMATTER_SIZE, 1 << 20);
    assert_eq!(MAX_FRONTMATTER_NODES, 200_000);
}

// ── Size cap (exact boundary, direct on the choke point) ────────────────────────

#[test]
fn c2_size_cap_exact_boundary() {
    // At the cap: a single ~1 MiB string value parses fine (PF-013 at-cap Ok twin).
    let at = parse_frontmatter_yaml(&fm_of_size(MAX_FRONTMATTER_SIZE));
    assert!(at.is_ok(), "at-cap frontmatter must parse: {at:?}");

    // One byte over: rejected as a resource limit, BEFORE any YAML work.
    let over = parse_frontmatter_yaml(&fm_of_size(MAX_FRONTMATTER_SIZE + 1));
    assert!(
        is_rl(&over),
        "over-cap must be a resource limit, got {over:?}"
    );
    let m = msg(&over);
    assert!(
        m.contains("frontmatter"),
        "message must mention frontmatter: {m}"
    );
    assert!(
        m.contains(&MAX_FRONTMATTER_SIZE.to_string()),
        "message must state the limit: {m}"
    );
    assert!(
        !m.contains("ZZSENTINELZZ"),
        "message must not echo the frontmatter content: {m}"
    );
}

#[test]
fn c3_size_cap_precedes_yaml_parse() {
    // Over the cap AND malformed (`k: [` never closes). The size guard must fire FIRST,
    // so this is a resource limit, not a YAML syntax error (order proof).
    let raw = format!("k: [{}", "x".repeat(MAX_FRONTMATTER_SIZE));
    let r = parse_frontmatter_yaml(&raw);
    assert!(is_rl(&r), "size cap must precede the parse: {r:?}");
    assert!(
        !is_yaml(&r),
        "must NOT surface as a YAML syntax error: {r:?}"
    );
}

#[test]
fn c1_at_cap_compiles_end_to_end() {
    // End-to-end wiring: a valid, comfortably-under-cap frontmatter compiles and the body
    // is preserved.
    let doc = wrap("greeting: hello\n");
    let out = crate::compile_str(&doc)
        .expect("valid frontmatter must compile")
        .into_markdown()
        .expect("markdown output");
    assert!(out.ends_with("Hi\n"), "body must be preserved: {out:?}");
}

#[test]
fn c2_size_cap_end_to_end() {
    // The cap is enforced on the string-compile path, not only in a unit call.
    let doc = wrap(&fm_of_size(MAX_FRONTMATTER_SIZE + 4096));
    let r = crate::check_str(&doc);
    assert!(
        is_rl(&r),
        "oversized frontmatter must be rejected via check_str: {r:?}"
    );
}

// ── Node budget (exact accounting on tiny documents) ────────────────────────────

#[test]
fn budget_scalar_accounting() {
    // map(1) + key(1) + scalar(1) = 3 nodes.
    let raw = "k: v\n";
    assert!(parse_frontmatter_yaml_bounded(raw, usize::MAX, 3).is_ok());
    assert!(is_rl(&parse_frontmatter_yaml_bounded(raw, usize::MAX, 2)));
}

#[test]
fn budget_sequence_accounting() {
    // map(1) + key(1) + seq(1) + 2 scalars = 5 nodes.
    let raw = "k: [x, x]\n";
    assert!(parse_frontmatter_yaml_bounded(raw, usize::MAX, 5).is_ok());
    assert!(is_rl(&parse_frontmatter_yaml_bounded(raw, usize::MAX, 4)));
}

#[test]
fn budget_keys_are_charged() {
    // Two entries: map(1) + [key(1)+scalar(1)] + [key(1)+scalar(1)] = 5 nodes. If keys
    // were NOT charged the count would be 3 and a budget of 3 would (wrongly) pass.
    let raw = "a: 1\nb: 2\n";
    assert!(parse_frontmatter_yaml_bounded(raw, usize::MAX, 5).is_ok());
    assert!(is_rl(&parse_frontmatter_yaml_bounded(raw, usize::MAX, 4)));
    assert!(is_rl(&parse_frontmatter_yaml_bounded(raw, usize::MAX, 3)));
}

#[test]
fn budget_tagged_accounting() {
    // tagged wrapper(1) + inner scalar(1) = 2 nodes (M11: the `!tag` wrapper is charged).
    let raw = "k: !T 5\n";
    // top is map(1) + key(1) + tagged(1) + scalar(1) = 4.
    assert!(parse_frontmatter_yaml_bounded(raw, usize::MAX, 4).is_ok());
    assert!(is_rl(&parse_frontmatter_yaml_bounded(raw, usize::MAX, 3)));
}

#[test]
fn c4_c5_node_budget_real_boundary() {
    // C-4: exactly at the node cap parses (at-cap Ok twin, PF-013).
    let at = alias_bomb_at(MAX_FRONTMATTER_NODES);
    let at_r = parse_frontmatter_yaml(&at);
    assert!(at_r.is_ok(), "exactly at the node cap must parse: {at_r:?}");

    // C-5: one node over the cap is rejected as a resource limit.
    let over = alias_bomb_at(MAX_FRONTMATTER_NODES + 1);
    let over_r = parse_frontmatter_yaml(&over);
    assert!(
        is_rl(&over_r),
        "one node over the cap must be rejected: {over_r:?}"
    );
    let m = msg(&over_r);
    assert!(
        m.contains(&MAX_FRONTMATTER_NODES.to_string()),
        "message must state the node limit: {m}"
    );
    assert!(
        !m.contains("*a"),
        "message must not echo the bomb content: {m}"
    );
}

/// Exactly-`target`-node alias bomb (wrapper around `bomb_with_nodes`).
fn alias_bomb_at(target: usize) -> String {
    bomb_with_nodes(target)
}

#[test]
fn c6_alias_revisits_are_counted() {
    // ~150 KB source, but each of the m aliases re-expands the n-element anchor, so the
    // materialised tree far exceeds the node cap: the budget counts re-visits, not bytes.
    let raw = alias_bomb(MAX_FRONTMATTER_NODES / 4, 4, 0);
    assert!(
        raw.len() < 400 * 1024,
        "bomb source stays small: {} bytes",
        raw.len()
    );
    let r = parse_frontmatter_yaml(&raw);
    assert!(
        is_rl(&r),
        "alias re-visits must be counted toward the budget: {r:?}"
    );
}

#[test]
fn c8_merge_key_is_a_plain_key_and_bomb_is_budgeted() {
    // `<<` is NOT applied as a merge key by serde_yaml_ng::from_str::<Value> — it is a
    // literal key. A `b: {<<: [*a, *a, ...]}` document is therefore just an alias vector,
    // and it is bounded by the node budget like any other.
    let anchor: Vec<String> = (0..2000).map(|i| format!("  k{i}: {i}")).collect();
    let refs = vec!["*a"; 200].join(", ");
    let raw = format!("a: &a\n{}\nb:\n  <<: [{refs}]\n", anchor.join("\n"));
    let r = parse_frontmatter_yaml(&raw);
    assert!(is_rl(&r), "`<<` alias bomb must be budgeted: {r:?}");
}

// ── Depth pins (existing serde / Value::from_yaml behaviour, via check_str) ──────

#[test]
fn c10a_flow_depth_64_ok() {
    let r = crate::check_str(&wrap(&nested_flow_seq(64)));
    assert!(r.is_ok(), "64-deep flow nest must be accepted: {r:?}");
}

#[test]
fn c10b_flow_depth_65_value_nesting() {
    let r = crate::check_str(&wrap(&nested_flow_seq(65)));
    assert!(is_yaml(&r), "65-deep must be a YAML error: {r:?}");
    assert!(
        msg(&r).contains("value nesting exceeds maximum depth of 64"),
        "expected value-nesting message: {}",
        msg(&r)
    );
}

#[test]
fn c10c_flow_depth_127_value_nesting_not_recursion() {
    let r = crate::check_str(&wrap(&nested_flow_seq(127)));
    assert!(is_yaml(&r), "127-deep must be a YAML error: {r:?}");
    let m = msg(&r);
    assert!(
        m.contains("value nesting exceeds maximum depth of 64"),
        "expected value-nesting message at 127: {m}"
    );
    assert!(
        !m.contains("recursion limit"),
        "127 is below serde's recursion limit; must not mention it: {m}"
    );
}

#[test]
fn c10d_flow_depth_128_recursion_limit() {
    let r = crate::check_str(&wrap(&nested_flow_seq(128)));
    assert!(is_yaml(&r), "128-deep must be a YAML error: {r:?}");
    assert!(
        msg(&r).contains("recursion limit exceeded"),
        "expected serde recursion-limit message at 128: {}",
        msg(&r)
    );
}

#[test]
fn c10e_block_map_depth_128_value_nesting() {
    let r = crate::check_str(&wrap(&nested_block_map(128)));
    assert!(
        is_yaml(&r),
        "128-deep block map must be a YAML error: {r:?}"
    );
    assert!(
        msg(&r).contains("value nesting exceeds maximum depth of 64"),
        "expected value-nesting message: {}",
        msg(&r)
    );
}

#[test]
fn c10f_tagged_nest_shallow_ok() {
    // Tagged values exercise visit_enum and, while comfortably under the depth-64 cap,
    // are accepted. Each nested `!t [...]` adds two levels (tag + sequence), so depth 20
    // and 30 map to value depths 40 and 60 — both under 64.
    assert!(crate::check_str(&wrap(&tagged_nest(20))).is_ok());
    assert!(crate::check_str(&wrap(&tagged_nest(30))).is_ok());
}

#[test]
fn c10g_flow_depth_10000_recursion_limit() {
    let r = crate::check_str(&wrap(&nested_flow_seq(10_000)));
    assert!(
        is_yaml(&r),
        "very deep flow nest must be a YAML error: {r:?}"
    );
    assert!(
        msg(&r).contains("recursion limit exceeded"),
        "expected serde recursion-limit message: {}",
        msg(&r)
    );
}

// ── Billion-laughs: serde's repetition limit, not our budget ────────────────────

#[test]
fn c11_billion_laughs_repetition_limit() {
    let r = parse_frontmatter_yaml(&laughs(5, 7));
    assert!(
        is_yaml(&r),
        "billion-laughs must surface as a YAML error: {r:?}"
    );
    assert!(
        msg(&r).contains("repetition limit exceeded"),
        "expected serde repetition-limit message: {}",
        msg(&r)
    );
}

#[test]
fn c11c_shallow_laughs_ok() {
    let r = parse_frontmatter_yaml(&laughs(5, 3));
    assert!(r.is_ok(), "shallow amplification must parse: {r:?}");
}

// ── Parity with the unbounded parser for non-attack inputs ──────────────────────

/// The budgeted parser must produce the SAME value (Ok) or the SAME error message (Err)
/// as `serde_yaml_ng::from_str::<Value>` for any input that is not an amplification
/// attack. For errors, the message is compared byte-for-byte against the raw parser's.
fn assert_parity(raw: &str) {
    let bounded = parse_frontmatter_yaml(raw);
    let plain = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(raw);
    match (&bounded, &plain) {
        (Ok(b), Ok(p)) => assert_eq!(b, p, "value parity for {raw:?}"),
        (Err(MdsError::YamlError { message }), Err(e)) => {
            assert_eq!(*message, e.to_string(), "error-message parity for {raw:?}");
        }
        _ => panic!(
            "parity mismatch for {raw:?}: bounded={bounded:?}, plain_is_err={}",
            plain.is_err()
        ),
    }
}

#[test]
fn c_par_matches_unbounded_parser() {
    // syntax error, duplicate key, tagged scalar/sequence, `<<` literal key, non-mapping.
    assert_parity("a: [\n");
    assert_parity("a: 1\na: 2\n");
    assert_parity("x: !Thing [1, 2]\n");
    assert_parity("x: !!str 5\n");
    assert_parity("a: &a 1\n<<: *a\n");
    assert_parity("just text\n");
}

#[test]
fn c_par_duplicate_key_message_is_byte_identical() {
    let raw = "a: 1\na: 2\n";
    let err = parse_frontmatter_yaml(raw).expect_err("duplicate key must be a YAML error");
    let MdsError::YamlError { message } = err else {
        panic!("duplicate key must be a YAML error, got {err:?}");
    };
    let plain_msg = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(raw)
        .unwrap_err()
        .to_string();
    assert!(
        plain_msg.contains("duplicate entry with key \"a\""),
        "sanity: raw parser reports duplicate-key text: {plain_msg}"
    );
    assert_eq!(
        message, plain_msg,
        "duplicate-key message must be byte-identical"
    );
}

#[test]
fn c_par_merge_key_is_literal() {
    // `<<` is not merged; it is a literal string key alongside the anchored value.
    let v = parse_frontmatter_yaml("a: &a 1\n<<: *a\n").expect("parses");
    let map = v.as_mapping().expect("mapping");
    assert!(
        map.contains_key(serde_yaml_ng::Value::String("<<".to_string())),
        "`<<` must be preserved as a literal key: {map:?}"
    );
}
