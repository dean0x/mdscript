//! Duplicate JSON object key detection for `--vars` files (#326).
//!
//! Implementation arrives in Phase 2 of the v0.4.3 action plan (step C1). This
//! module currently contains only its test specifications — the items the tests
//! reference (`duplicate_json_keys`, `DuplicateKeys`, `MAX_DUPLICATE_KEY_PATHS`) do
//! not exist yet, so the crate's test build is intentionally RED until Phase 2
//! lands. See `.devflow/docs/handoff-v043-action-plan.md` step C1 for the design.

#[cfg(test)]
mod tests {
    use super::*;

    fn dup(json: &str) -> DuplicateKeys {
        duplicate_json_keys(json).expect("expected the fixture to parse")
    }

    /// Fixture shared by [`clean_document_reports_no_duplicates`] (T6) and
    /// [`every_json_leaf_shape_is_accepted`] (T9): one instance of every JSON leaf
    /// shape the visitor must handle, with no duplicates. The `\n` and `A` are
    /// JSON string escapes present in the JSON text itself — ASCII backslash
    /// sequences inside this Rust raw string, never live bytes (PF-018).
    const EVERY_LEAF_SHAPE_FIXTURE: &str = r#"{"nul":null,"t":true,"f":false,"neg":-1,"big":18446744073709551615,"flt":1.5e300,"s":"a\nbA","arr":[],"obj":{}}"#;

    // T1
    #[test]
    fn flat_duplicate_is_reported_once() {
        let d = dup(r#"{"x":1,"x":2}"#);
        assert_eq!(d.paths, vec!["x".to_string()]);
        assert_eq!(d.omitted, 0);
    }

    // T2
    #[test]
    fn nested_duplicate_reports_dotted_path() {
        let d = dup(r#"{"x":{"a":1,"a":2}}"#);
        assert_eq!(d.paths, vec!["x.a".to_string()]);
    }

    // T3
    #[test]
    fn duplicate_inside_array_element_reports_bracket_index() {
        let d = dup(r#"{"x":[{"k":0},{"k":0},{"a":1,"a":2}]}"#);
        assert_eq!(d.paths, vec!["x[2].a".to_string()]);
    }

    // T4
    #[test]
    fn duplicate_two_levels_under_an_array_element() {
        let d = dup(r#"{"x":[{"y":{"a":1,"a":2}}]}"#);
        assert_eq!(d.paths, vec!["x[0].y.a".to_string()]);
    }

    // T5
    #[test]
    fn triple_repeat_reports_one_entry() {
        let d = dup(r#"{"x":1,"x":2,"x":3}"#);
        assert_eq!(d.paths, vec!["x".to_string()]);
    }

    // T6 — positive control (PF-013) for every duplicate-detecting test in this
    // module: a document with no duplicates at all must report none.
    #[test]
    fn clean_document_reports_no_duplicates() {
        let d = dup(EVERY_LEAF_SHAPE_FIXTURE);
        assert!(
            d.paths.is_empty(),
            "expected no duplicates, got {:?}",
            d.paths
        );
        assert_eq!(d.omitted, 0);
    }

    // T7
    #[test]
    fn duplicates_are_reported_in_encounter_order() {
        let d = dup(r#"{"a":1,"b":{"c":1,"c":2},"a":3}"#);
        assert_eq!(d.paths, vec!["b.c".to_string(), "a".to_string()]);
    }

    // T8
    #[test]
    fn same_key_in_two_objects_is_two_paths() {
        let d = dup(r#"{"o":{"a":1,"a":2},"p":{"a":1,"a":2}}"#);
        assert_eq!(d.paths, vec!["o.a".to_string(), "p.a".to_string()]);
    }

    // T9
    #[test]
    fn every_json_leaf_shape_is_accepted() {
        let d = dup(EVERY_LEAF_SHAPE_FIXTURE);
        assert!(
            d.paths.is_empty(),
            "expected no duplicates, got {:?}",
            d.paths
        );
        // Non-vacuity: the same text really does have 9 distinct top-level keys —
        // otherwise an empty/trivial fixture would trivially pass T6/T9 both.
        let value: serde_json::Value =
            serde_json::from_str(EVERY_LEAF_SHAPE_FIXTURE).expect("fixture must be valid JSON");
        let serde_json::Value::Object(map) = value else {
            panic!("fixture must be a JSON object");
        };
        assert_eq!(map.len(), 9, "fixture must have exactly 9 top-level keys");
    }

    // T10 — pins the documented limitation: a key containing '.' cannot be
    // distinguished from a nesting separator in the rendered path.
    #[test]
    fn a_key_containing_a_dot_renders_ambiguously() {
        let d = dup(r#"{"a.b":1,"a.b":2}"#);
        assert_eq!(d.paths, vec!["a.b".to_string()]);
    }

    /// Generate a flat JSON object with `n` distinct keys (`k0`..`k{n-1}`), each
    /// key written twice (duplicated), in a single top-level object.
    fn generate_duplicated_keys(n: usize) -> String {
        let mut s = String::from("{");
        for i in 0..n {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(r#""k{i}":0,"k{i}":1"#));
        }
        s.push('}');
        s
    }

    // T11a
    #[test]
    fn recorded_paths_are_capped_and_the_rest_counted() {
        let json = generate_duplicated_keys(1_003);
        let d = dup(&json);
        assert_eq!(d.paths.len(), MAX_DUPLICATE_KEY_PATHS);
        assert_eq!(d.omitted, 3);
    }

    // T11b — non-vacuity for T11a: below the cap, nothing is omitted and every
    // path is kept.
    #[test]
    fn paths_below_the_cap_are_all_kept() {
        let json = generate_duplicated_keys(999);
        let d = dup(&json);
        assert_eq!(d.paths.len(), 999);
        assert_eq!(d.omitted, 0);
    }

    /// Build `depth` nested single-key objects (`n0`..`n{depth-1}`) wrapping a
    /// duplicated `dup` key at the deepest level.
    fn nested_objects_with_duplicate_at_depth(depth: usize) -> String {
        let mut open = String::new();
        let mut close = String::new();
        for i in 0..depth {
            open.push_str(&format!(r#"{{"n{i}":"#));
            close.push('}');
        }
        format!(r#"{open}{{"dup":1,"dup":2}}{close}"#)
    }

    // T12
    #[test]
    fn nesting_at_the_serde_json_limit_is_scanned() {
        // 120 nested objects — below serde_json's 128-level recursion limit — with
        // a duplicate at the deepest level.
        let json = nested_objects_with_duplicate_at_depth(120);
        let d = dup(&json);
        assert_eq!(d.paths.len(), 1);
        let expected_path = (0..120)
            .map(|i| format!("n{i}"))
            .collect::<Vec<_>>()
            .join(".")
            + ".dup";
        assert_eq!(d.paths[0], expected_path);
    }

    // T13 — proves D7's recursion bound is real: an error, never a panic.
    #[test]
    fn nesting_beyond_the_serde_json_limit_is_an_error_not_a_panic() {
        // 129 nested arrays exceeds serde_json's 128-level recursion bound.
        let mut json = "[".repeat(129);
        json.push_str(&"]".repeat(129));
        let err = duplicate_json_keys(&json).expect_err("expected a recursion-limit error");
        assert!(
            err.to_string().contains("recursion limit exceeded"),
            "expected a recursion-limit message, got: {err}"
        );
    }

    // T14a
    #[test]
    fn malformed_json_is_an_error() {
        assert!(duplicate_json_keys("{").is_err());
    }

    // T14b — proves `de.end()` is called: trailing garbage after a complete value
    // must be rejected, not silently ignored.
    #[test]
    fn trailing_data_is_an_error() {
        assert!(duplicate_json_keys("{} {}").is_err());
    }

    // T15
    #[test]
    fn an_array_root_reports_index_prefixed_paths() {
        let d = dup(r#"[{"a":1,"a":2}]"#);
        assert_eq!(d.paths, vec!["[0].a".to_string()]);
    }
}
