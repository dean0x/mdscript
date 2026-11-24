//! Compare the data YAML funnel with the JSON parser before conversion to MDS values.

use serde_json::Value;

use super::{parse_data_yaml_with, Allowance};
use crate::error::MdsError;
use crate::value::MAX_VALUE_DEPTH;

fn yaml_as_json(raw: &str) -> Result<Value, MdsError> {
    let (yaml, _) = parse_data_yaml_with(raw, Allowance::PER_PARSE)?;
    Ok(serde_json::to_value(yaml).expect("the parsed tree must be representable as JSON"))
}

fn assert_parity(raw: &str) {
    let expected =
        serde_json::from_str::<Value>(raw).expect("the JSON oracle must accept the case");
    let actual = yaml_as_json(raw).expect("the data YAML funnel must accept the case");
    assert_eq!(actual, expected);
}

#[test]
fn json_tab_indentation_preserves_the_value() {
    assert_parity("{\n\t\"key\": [true, null]\n}");
}

#[test]
#[ignore = "the YAML parser rejects a valid JSON key of 1025 characters"]
fn json_key_past_1024_characters_preserves_the_value() {
    assert_parity(&format!(r#"{{"{}":42}}"#, "k".repeat(1025)));
}

#[test]
fn json_escaped_slash_preserves_the_value() {
    assert_parity(r#"{"path":"a\/b"}"#);
}

#[test]
#[ignore = "the YAML parser rejects a valid JSON surrogate pair"]
fn json_surrogate_pair_preserves_the_value() {
    let raw = concat!(r#"{"text":"\u"#, r#"D83D\u"#, r#"DE80"}"#);
    assert_parity(raw);
}

#[test]
fn json_uppercase_exponent_preserves_the_value() {
    assert_parity("1E2");
}

#[test]
#[ignore = "the YAML parser changes JSON minus zero to unsigned zero"]
fn json_minus_zero_preserves_the_sign() {
    let expected = serde_json::from_str::<Value>("-0").unwrap();
    let actual = yaml_as_json("-0").unwrap();
    assert_eq!(actual, expected);
    assert_eq!(
        actual.as_f64().unwrap().to_bits(),
        expected.as_f64().unwrap().to_bits()
    );
}

#[test]
#[ignore = "the YAML parser accepts an out-of-range JSON exponent as a string"]
fn both_parsers_refuse_a_json_exponent_out_of_range() {
    assert_parity("1e308");
    assert!(serde_json::from_str::<Value>("1e400").is_err());
    let actual = yaml_as_json("1e400");
    assert!(
        matches!(actual, Err(MdsError::YamlError { .. })),
        "the data YAML funnel must refuse the out-of-range number, got {actual:?}"
    );
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
fn data_yaml_refuses_duplicate_json_keys() {
    assert_parity(r#"{"key":1,"other":2}"#);
    let raw = r#"{"key":1,"key":2}"#;
    assert_eq!(serde_json::from_str::<Value>(raw).unwrap()["key"], 2);
    let err = parse_data_yaml_with(raw, Allowance::PER_PARSE).unwrap_err();
    assert!(matches!(err, MdsError::YamlError { .. }));
    assert!(err.to_string().contains("duplicate entry"), "{err}");
}
