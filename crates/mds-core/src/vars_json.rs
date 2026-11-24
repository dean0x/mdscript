//! JSON vars reader and shared duplicate-key detection.
//!
//! The `serde_json::Value` reader keeps the last value for a repeated key.
//! Vars readers use a second pass to report repeats without changes to those
//! values. Data sources use the same scan before tree construction. They
//! refuse the first repeat and limit the nodes, bytes and depth of the tree.
//!
//! # Path grammar
//!
//! A reported path names a JSON key by walking from the document root:
//! - Object nesting is dotted: `x.a`.
//! - Array-element nesting uses a 0-based bracket index: `x[2].a`, and an array at
//!   the document root renders as `[0].a`. This scanner produces that array-root
//!   form on any JSON text (see the `T15` test below); in practice, though, the
//!   public `load_vars_file`/`load_vars_str` (and their `_reporting_duplicates`
//!   variants, in `lib.rs`) reject a non-object top-level value before this scan
//!   ever runs, so a caller of those functions never observes it — only paths
//!   starting with a key are reachable through the public API.
//! - A literal key containing `.`, `[`, or `]` is **not** escaped — it renders
//!   ambiguously with an actual nesting separator, and an empty-string key
//!   renders as an empty segment. This is a documented, accepted limitation
//!   (display-only; the underlying key is never altered).
//!
//! # Cap
//!
//! At most [`MAX_DUPLICATE_KEY_PATHS`] paths are recorded; any further distinct
//! duplicate path is counted in [`DuplicateKeys::omitted`] instead. One path is
//! recorded per key, however many times that key repeats within its enclosing
//! object (a key appearing 3 times still yields one path).
//!
//! # Recursion bound
//!
//! The JSON deserializer limits recursion to 128 levels. The data scan also
//! limits value depth to 64 before it visits a child. Vars readers apply the
//! value depth limit before they call the duplicate-key scan.

use std::collections::HashSet;
use std::fmt;
use std::fmt::Write as _;
use std::sync::Arc;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

/// Maximum number of distinct duplicate-key paths recorded by
/// [`duplicate_json_keys`]. Mirrors the precedent of `MAX_WARNINGS`
/// (`evaluator.rs`) and `MAX_DIAGNOSTICS` (`limits.rs`): a hostile document with
/// many thousands of duplicate keys must not produce an unbounded warning flood.
/// Paths beyond the cap are counted, not recorded — see [`DuplicateKeys::omitted`].
pub(crate) const MAX_DUPLICATE_KEY_PATHS: usize = 1_000;

/// Result of scanning a JSON document's text for duplicate object keys.
///
/// `paths` lists each duplicated key's rendered path (see the module doc's "Path
/// grammar" section), in encounter order, one entry per path regardless of how
/// many times the key repeats, capped at [`MAX_DUPLICATE_KEY_PATHS`]. `omitted`
/// counts any further distinct duplicate paths beyond the cap.
#[derive(Debug)]
pub(crate) struct DuplicateKeys {
    pub(crate) paths: Vec<String>,
    pub(crate) omitted: usize,
}

/// One segment of the path to the object currently being scanned: a named object
/// key, or a 0-based array index.
enum Seg {
    Key(Arc<str>),
    Index(usize),
}

/// Scan state threaded through the recursive visitor: the path to the object
/// currently being visited, the duplicate paths found so far, and the count of
/// duplicates omitted past the cap.
#[derive(Default)]
struct Scan<'a> {
    path: Vec<Seg>,
    found: Vec<String>,
    omitted: usize,
    data: Option<&'a dyn Fn(usize) -> bool>,
    rejection: Option<DataScanError>,
}

#[derive(Clone, Copy)]
pub(crate) enum DataScanError {
    Invalid,
    Duplicate,
    Depth,
}

impl Scan<'_> {
    fn charge<E: de::Error>(&self, len: usize) -> Result<(), E> {
        if self.data.is_some_and(|charge| !charge(len)) {
            return Err(E::custom("data JSON budget exhausted"));
        }
        Ok(())
    }

    fn reject<E: de::Error>(&mut self, reason: DataScanError) -> E {
        self.rejection = Some(reason);
        E::custom("invalid data JSON")
    }

    /// Record one duplicate occurrence of `key` inside the object at the current
    /// `path`. Renders the full path (container path + `key`) into a single
    /// `String` via `write!`/`push_str` — no per-segment `format!` allocation
    /// chain. Bounded: once [`MAX_DUPLICATE_KEY_PATHS`] paths have been recorded,
    /// every further call only increments `omitted`.
    fn record(&mut self, key: &str) {
        if self.found.len() >= MAX_DUPLICATE_KEY_PATHS {
            self.omitted += 1;
            return;
        }
        let mut rendered = String::new();
        for seg in &self.path {
            match seg {
                Seg::Key(k) => {
                    if !rendered.is_empty() {
                        rendered.push('.');
                    }
                    rendered.push_str(k);
                }
                Seg::Index(i) => {
                    // write! into an existing String never allocates a throwaway
                    // intermediate — the digits are appended in place.
                    let _ = write!(rendered, "[{i}]");
                }
            }
        }
        if !rendered.is_empty() {
            rendered.push('.');
        }
        rendered.push_str(key);
        self.found.push(rendered);
    }
}

/// Value-free visitor/seed pair: recurses through a JSON document recording
/// duplicate object keys, without building a `Value`. Holds a reborrowed `&mut
/// Scan` so the same scan state threads through every recursive call.
struct DupScan<'a, 'budget> {
    scan: &'a mut Scan<'budget>,
}

// Charge decoded key bytes before retaining a copy. The path and the set
// share the same allocation.
struct Key<'a, 'budget>(&'a Scan<'budget>);

impl<'de> DeserializeSeed<'de> for Key<'_, '_> {
    type Value = Arc<str>;

    fn deserialize<D: de::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        de.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for Key<'_, '_> {
    type Value = Arc<str>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON object key")
    }

    fn visit_str<E: de::Error>(self, key: &str) -> Result<Self::Value, E> {
        self.0.charge(key.len())?;
        Ok(Arc::from(key))
    }
}

impl<'de> DeserializeSeed<'de> for DupScan<'_, '_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<(), D::Error>
    where
        D: de::Deserializer<'de>,
    {
        if self.scan.data.is_some() && self.scan.path.len() > crate::value::MAX_VALUE_DEPTH {
            return Err(self.scan.reject(DataScanError::Depth));
        }
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for DupScan<'_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any valid JSON value")
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        self.scan.charge(0)
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        self.scan.charge(0)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        self.scan.charge(0)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        self.scan.charge(0)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        self.scan.charge(0)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<(), E> {
        self.scan.charge(v.len())
    }

    fn visit_i128<E: de::Error>(self, _: i128) -> Result<(), E> {
        self.scan.charge(0)
    }

    fn visit_u128<E: de::Error>(self, _: u128) -> Result<(), E> {
        self.scan.charge(0)
    }

    fn visit_none<E: de::Error>(self) -> Result<(), E> {
        self.scan.charge(0)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let Self { scan } = self;
        scan.charge(0)?;
        let mut i = 0usize;
        loop {
            scan.path.push(Seg::Index(i));
            // Reborrow: `&mut *scan` yields a fresh `&mut Scan` for this element
            // without moving `scan` out of the outer closure, so the loop can
            // keep using it on the next iteration.
            let got = seq.next_element_seed(DupScan { scan: &mut *scan });
            scan.path.pop();
            if got?.is_none() {
                return Ok(());
            }
            // Bounded by the document itself (ultimately by MAX_FILE_SIZE on the
            // caller side): a JSON array literal cannot have more elements than
            // there are bytes to spell them.
            i += 1;
        }
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let Self { scan } = self;
        scan.charge(0)?;
        let mut seen = HashSet::new();
        let mut reported = HashSet::new();
        while let Some(key) = map.next_key_seed(Key(scan))? {
            if !seen.insert(Arc::clone(&key)) {
                if scan.data.is_some() {
                    return Err(scan.reject(DataScanError::Duplicate));
                }
                if reported.insert(Arc::clone(&key)) {
                    scan.record(&key);
                }
            }
            scan.path.push(Seg::Key(key));
            let v = map.next_value_seed(DupScan { scan: &mut *scan });
            scan.path.pop();
            v?;
        }
        Ok(())
    }
}

/// Scan `json` for JSON object keys that repeat within their enclosing object, at
/// any depth, without building a `serde_json::Value`.
///
/// Returns the rendered path of each duplicated key (see the module doc's "Path
/// grammar" section), in encounter order, capped at [`MAX_DUPLICATE_KEY_PATHS`]
/// with any excess counted in [`DuplicateKeys::omitted`].
///
/// # Errors
///
/// Returns `Err` when `json` is not valid JSON, or when nesting exceeds
/// `serde_json`'s built-in recursion limit (128 levels) — never panics. Callers
/// in this crate pass text a `serde_json::from_str::<Value>` call has already
/// accepted, so an error here is a divergence between the two passes and is
/// propagated rather than swallowed.
pub(crate) fn duplicate_json_keys(json: &str) -> Result<DuplicateKeys, serde_json::Error> {
    let mut scan = Scan::default();
    scan_json(json, &mut scan)?;
    Ok(DuplicateKeys {
        paths: scan.found,
        omitted: scan.omitted,
    })
}

pub(crate) fn parse_vars_json(json: &str) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::from_str(json)
}

/// Refuse repeated keys and bound the data tree before its construction.
pub(crate) fn check_data_json(
    json: &str,
    charge: &dyn Fn(usize) -> bool,
) -> Result<(), DataScanError> {
    let mut scan = Scan {
        data: Some(charge),
        ..Scan::default()
    };
    scan_json(json, &mut scan).map_err(|_| scan.rejection.unwrap_or(DataScanError::Invalid))
}

fn scan_json(json: &str, scan: &mut Scan<'_>) -> Result<(), serde_json::Error> {
    let mut de = serde_json::Deserializer::from_str(json);
    DupScan { scan }.deserialize(&mut de)?;
    // Require one complete document.
    de.end()
}

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
