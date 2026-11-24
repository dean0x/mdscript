use super::{convert, Accept, DataLabel};
use crate::error::MdsError;
use crate::evaluator::budget::{value_bytes, MemBudget, Part};
use crate::limits::MAX_DATA_FILE_BYTES;
use crate::value::{Value, MAX_VALUE_DEPTH};

const LABEL: DataLabel<'static> = DataLabel {
    module: "entry.mds",
    entry: "settings",
};

fn read(raw: &str, accept: Accept) -> Result<Value, MdsError> {
    convert(raw.as_bytes(), accept, LABEL, &mut MemBudget::default())
}

fn data_error(result: Result<Value, MdsError>) -> String {
    let err = result.unwrap_err();
    assert!(matches!(err, MdsError::Data { .. }), "{err}");
    let message = err.to_string();
    assert!(message.contains("entry \"settings\""), "{message}");
    message
}

fn escape(code: u32) -> String {
    format!("{}u{code:04X}", char::from(92))
}

#[test]
fn class_checks_use_decoded_keys_and_values() {
    for accept in [Accept::Yaml, Accept::Json] {
        assert!(read(r#"{"ok\t\n":"\t\n"}"#, accept).is_ok());
        for code in [0x1b, 0x202e, 0x2028] {
            let hex = escape(code);
            for (raw, path) in [
                (
                    format!(r#"{{"outer":[{{"inner":"{hex}"}}]}}"#),
                    "outer[0].inner".to_owned(),
                ),
                (
                    format!(r#"{{"outer":{{"bad{hex}":0}}}}"#),
                    format!("outer.bad{hex}"),
                ),
            ] {
                let message = data_error(read(&raw, accept));
                assert!(message.contains(&path), "{message} must contain {path}");
                assert!(
                    !message.contains(char::from_u32(code).unwrap()),
                    "{message}"
                );
            }
        }
    }
    let raw = format!("a: \"{}\"", char::from(27));
    data_error(read(&raw, Accept::Yaml));
}

#[test]
fn only_crlf_in_values_normalizes_to_lf() {
    for accept in [Accept::Yaml, Accept::Json] {
        assert_eq!(read(r#""a\r\nb""#, accept).unwrap(), Value::from("a\nb"));
        assert_eq!(read(r#""a\nb""#, accept).unwrap(), Value::from("a\nb"));
        data_error(read(r#""a\rb""#, accept));
        data_error(read(r#"{"a\r\nb": 0}"#, accept));
    }
    assert_eq!(read("a\r\nb", Accept::Text).unwrap(), Value::from("a\nb"));
    data_error(read("a\rb", Accept::Text));
}

const NUMBER_RANGE: &str = "-2^53 to 2^53";
const ABOVE_U128: &str = "340282366920938463463374607431768211457";
const BELOW_I128: &str = "-170141183460469231731687303715884105729";

#[test]
fn data_numbers_refuse_integers_past_the_limit_before_rounding() {
    for accept in [Accept::Yaml, Accept::Json] {
        for (raw, expected) in [
            ("9007199254740992", 9_007_199_254_740_992.0),
            ("-9007199254740992", -9_007_199_254_740_992.0),
        ] {
            assert_eq!(read(raw, accept).unwrap(), Value::Number(expected));
        }
        for raw in ["9007199254740993", "-9007199254740993"] {
            let message = data_error(read(raw, accept));
            assert!(message.contains(NUMBER_RANGE), "{message}");
        }
    }
}

#[test]
fn data_numbers_refuse_integers_outside_the_64_bit_range() {
    for raw in ["18446744073709551617", "-9223372036854775809"] {
        // The YAML visitor refuses 128-bit integers before conversion.
        data_error(read(raw, Accept::Yaml));
        let message = data_error(read(&format!(r#"{{"amount":{raw}}}"#), Accept::Json));
        assert!(message.contains("amount"), "{message}");
        assert!(message.contains(NUMBER_RANGE), "{message}");
    }
}

#[test]
fn data_numbers_refuse_finite_floats_past_the_limit() {
    for accept in [Accept::Yaml, Accept::Json] {
        assert_eq!(read("1.5", accept).unwrap(), Value::Number(1.5));
        for raw in ["1e300", "-1e300"] {
            let message = data_error(read(raw, accept));
            assert!(message.contains(NUMBER_RANGE), "{message}");
        }
    }
}

#[test]
fn data_numbers_refuse_integers_that_parse_as_floats_in_both_formats() {
    for accept in [Accept::Yaml, Accept::Json] {
        for raw in [ABOVE_U128, BELOW_I128] {
            let message = data_error(read(raw, accept));
            assert!(message.contains(NUMBER_RANGE), "{raw}: {message}");
        }
    }
}

#[test]
fn frontmatter_numbers_past_the_data_limit_keep_their_values() {
    for (raw, expected) in [
        ("9007199254740993", 9_007_199_254_740_992.0),
        (ABOVE_U128, 2.0_f64.powi(128)),
        (BELOW_I128, -2.0_f64.powi(127)),
        ("1e300", 1e300),
    ] {
        let frontmatter = crate::resolver::parse_frontmatter_yaml(raw).unwrap();
        assert_eq!(
            Value::from_yaml_ref(&frontmatter).unwrap(),
            Value::Number(expected),
            "{raw}"
        );
    }
}

#[test]
fn non_finite_yaml_numbers_fail() {
    assert_eq!(read("1.5", Accept::Yaml).unwrap(), Value::Number(1.5));
    for raw in [".inf", "-.inf", ".nan"] {
        data_error(read(raw, Accept::Yaml));
    }
}

#[test]
fn one_document_only_and_yaml_tags_are_accepted() {
    assert_eq!(
        read("!local 42", Accept::Yaml).unwrap(),
        Value::Number(42.0)
    );
    data_error(read("42\n---\n43", Accept::Yaml));
    data_error(read("a: 1\na: 2", Accept::Yaml));
    data_error(read("42 43", Accept::Json));
    data_error(read("{\"a\":1 # YAML only\n}", Accept::Json));
}

#[test]
fn utf8_and_one_leading_bom_apply_to_every_format() {
    let bom = char::from_u32(0xfeff).unwrap();
    for accept in [Accept::Yaml, Accept::Json, Accept::Text] {
        let control = read("42", accept).unwrap();
        assert_eq!(read(&format!("{bom}42"), accept).unwrap(), control);
        data_error(read(&format!("{bom}{bom}42"), accept));
        data_error(convert(&[0xff], accept, LABEL, &mut MemBudget::default()));
    }
}

#[test]
fn json_byte_cap_includes_the_bom_and_precedes_decode() {
    let cap = MAX_DATA_FILE_BYTES as usize;
    let mut raw = vec![b' '; cap];
    raw[0] = b'0';
    assert_eq!(
        convert(&raw, Accept::Json, LABEL, &mut MemBudget::default()).unwrap(),
        Value::Number(0.0)
    );
    raw.push(0xff);
    let err = convert(&raw, Accept::Json, LABEL, &mut MemBudget::default()).unwrap_err();
    assert!(matches!(err, MdsError::ResourceLimit { .. }), "{err}");
    assert!(err.to_string().contains("data file too large"), "{err}");
}

#[test]
fn data_value_depth_accepts_the_cap_and_refuses_one_more_level() {
    for accept in [Accept::Yaml, Accept::Json] {
        let nested = |depth: usize| format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        assert!(read(&nested(MAX_VALUE_DEPTH), accept).is_ok());
        assert!(data_error(read(&nested(MAX_VALUE_DEPTH + 1), accept)).contains("nesting"));
    }
}

#[test]
fn conversion_releases_temporary_bytes_and_keeps_only_value_bytes() {
    for (accept, raw) in [
        (Accept::Yaml, "outer: [null]"),
        (Accept::Json, r#"{"outer":[null]}"#),
        (Accept::Text, "outer"),
    ] {
        let mut mem = MemBudget::default();
        mem.charge(Part::Temp, Some(17)).unwrap();
        mem.charge(Part::Buffers, Some(23)).unwrap();
        let value = convert(raw.as_bytes(), accept, LABEL, &mut mem).unwrap();
        assert_eq!(mem.buffers, 23 + value_bytes(&value).unwrap());
        assert_eq!(mem.mark(), 17);
    }
}

#[test]
fn conversion_failure_keeps_no_data_charge() {
    for (accept, raw) in [
        (Accept::Yaml, "[null]"),
        (Accept::Json, "[null]"),
        (Accept::Text, "text"),
    ] {
        let mut mem = MemBudget::default();
        assert!(convert(raw.as_bytes(), accept, LABEL, &mut mem).is_ok());
        mem.headroom = mem.buffers;
        let before = mem;
        let err = convert(raw.as_bytes(), accept, LABEL, &mut mem).unwrap_err();
        assert!(matches!(err, MdsError::ResourceLimit { .. }), "{err}");
        assert_eq!(mem, before);
    }
}
