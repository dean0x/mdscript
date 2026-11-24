//! Compare the JSON data funnel with the JSON parser before conversion to MDS values.

use serde_json::Value;

use super::{convert, Accept, DataLabel};
use crate::error::MdsError;
use crate::evaluator::budget::MemBudget;
use crate::limits::{MAX_DATA_FILE_BYTES, MAX_DATA_NODES_PER_FILE};
use crate::resolver::{parse_data_json_with, Allowance, NODE_YAML};
use crate::value::MAX_VALUE_DEPTH;

const LABEL: DataLabel<'static> = DataLabel {
    module: "entry.mds",
    entry: "sample",
    file: None,
};

fn parsed_json(raw: &str) -> Result<Value, MdsError> {
    let (tree, _) = parse_data_json_with(raw, Allowance::in_compile(&MemBudget::default()), LABEL)?;
    Ok(serde_json::to_value(tree).expect("the parsed tree must be representable as JSON"))
}

fn assert_parity(raw: &str) {
    let expected =
        serde_json::from_str::<Value>(raw).expect("the JSON oracle must accept the case");
    let actual = parsed_json(raw).expect("the JSON data funnel must accept the case");
    assert_eq!(actual, expected);
}

#[test]
fn json_tab_indentation_preserves_the_value() {
    assert_parity("{\n\t\"key\": [true, null]\n}");
}

#[test]
fn json_key_past_1024_characters_preserves_the_value() {
    assert_parity(&format!(r#"{{"{}":42}}"#, "k".repeat(1025)));
}

#[test]
fn json_escaped_slash_preserves_the_value() {
    assert_parity(r#"{"path":"a\/b"}"#);
}

#[test]
fn json_surrogate_pair_preserves_the_value() {
    let raw = concat!(r#"{"text":"\u"#, r#"D83D\u"#, r#"DE80"}"#);
    assert_parity(raw);
}

#[test]
fn json_uppercase_exponent_preserves_the_value() {
    assert_parity("1E2");
}

#[test]
fn json_minus_zero_preserves_the_sign() {
    let expected = serde_json::from_str::<Value>("-0").unwrap();
    let actual = parsed_json("-0").unwrap();
    assert_eq!(actual, expected);
    // Signed zero is finite. Keep its sign through the data conversion, too.
    let actual = convert(b"-0", Accept::Json, LABEL, &mut MemBudget::default()).unwrap();
    let crate::Value::Number(number) = actual else {
        panic!("expected a number")
    };
    assert_eq!(number.to_bits(), expected.as_f64().unwrap().to_bits());
}

#[test]
fn both_parsers_refuse_a_json_exponent_out_of_range() {
    assert_parity("1e308");
    assert!(serde_json::from_str::<Value>("1e400").is_err());
    // 1e400 cannot be a finite f64. Refuse it as data, not as a string.
    let actual = convert(b"1e400", Accept::Json, LABEL, &mut MemBudget::default());
    assert!(matches!(actual, Err(MdsError::Data { .. })), "{actual:?}");
}

#[test]
fn json_integer_past_the_exact_f64_limit_stays_exact_before_conversion() {
    assert_parity("9007199254740993");
    assert_parity("-9007199254740993");
}

#[test]
fn json_nested_at_the_data_value_depth_limit_preserves_the_value() {
    assert_parity(&format!(
        "{}0{}",
        "[".repeat(MAX_VALUE_DEPTH),
        "]".repeat(MAX_VALUE_DEPTH)
    ));
}

#[test]
fn json_with_one_leading_bom_preserves_the_value_after_decode() {
    let bom = char::from_u32(0xfeff).unwrap();
    let raw = format!("{bom}{{\"key\":42}}");
    // The data reader strips one leading BOM before either parser sees the text.
    assert_parity(raw.strip_prefix(bom).unwrap());
}

#[test]
fn json_empty_containers_preserve_their_types() {
    for raw in ["{}", "[]", r#"{"object":{},"array":[]}"#] {
        assert_parity(raw);
    }
}

#[test]
fn data_json_refuses_duplicate_decoded_keys() {
    assert_parity(r#"{"key":1,"other":2}"#);
    let raw = concat!(r#"{"key":1,"k\u"#, r#"0065y":2}"#);
    assert_eq!(serde_json::from_str::<Value>(raw).unwrap()["key"], 2);
    let err = parsed_json(raw).unwrap_err();
    assert!(matches!(err, MdsError::Data { .. }));
    assert!(
        err.to_string().contains("duplicate JSON object key"),
        "{err}"
    );
}

#[test]
fn json_funnel_counts_the_root_keys_and_elements() {
    let cap = MAX_DATA_NODES_PER_FILE;
    let raw = format!("{{\"key\":[{}0]}}", "0,".repeat(cap - 4));
    assert_eq!(
        parsed_json(&raw).unwrap()["key"].as_array().unwrap().len(),
        cap - 3
    );
    let raw = format!("{{\"key\":[{}0]}}", "0,".repeat(cap - 3));
    let err = parsed_json(&raw).unwrap_err();
    assert!(matches!(err, MdsError::ResourceLimit { .. }));
    assert!(err.to_string().contains("JSON node count"), "{err}");
}

#[test]
fn json_funnel_checks_source_bytes_before_parse() {
    let cap = MAX_DATA_FILE_BYTES as usize;
    let raw = format!("0{}", " ".repeat(cap - 1));
    assert_eq!(parsed_json(&raw).unwrap(), 0);
    let err = parsed_json(&format!("{raw}x")).unwrap_err();
    assert!(matches!(err, MdsError::ResourceLimit { .. }));
    assert!(err.to_string().contains("data file too large"), "{err}");
}

#[test]
fn json_funnel_bounds_live_tree_bytes() {
    let mut mem = MemBudget::default();
    let raw = r#""value""#;
    let cost = NODE_YAML + "value".len();
    mem.headroom = cost;
    let (_, spent) = parse_data_json_with(raw, Allowance::in_compile(&mem), LABEL).unwrap();
    assert_eq!(spent, cost);
    mem.headroom -= 1;
    let err = parse_data_json_with(raw, Allowance::in_compile(&mem), LABEL).unwrap_err();
    assert!(matches!(err, MdsError::ResourceLimit { .. }));
    assert!(err.to_string().contains("compile memory"), "{err}");
}

#[test]
fn json_funnel_does_not_count_brackets_in_strings_as_nesting() {
    let brackets = "[".repeat(1025);
    assert_parity(&format!(r#"["{brackets}"]"#));
    let raw = format!("{brackets}0{}", "]".repeat(brackets.len()));
    let err = parsed_json(&raw).unwrap_err();
    assert!(matches!(err, MdsError::Data { .. }));
    assert!(err.to_string().contains("nesting exceeds"), "{err}");
}
