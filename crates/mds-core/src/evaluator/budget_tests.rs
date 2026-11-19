use super::{bytes, display_len, render_value, value_bytes, MemBudget, Part, NODE, VALUE_ROOT};
use crate::{
    error::MdsError,
    limits::MAX_COMPILE_MEMORY,
    value::{Value, MAX_VALUE_DEPTH},
};

#[test]
fn charges_allow_exactly_the_limit() {
    let mut mem = MemBudget::default();
    mem.charge(Part::Buffers, MAX_COMPILE_MEMORY - 1).unwrap();
    mem.charge(Part::Temp, 1).unwrap();
    let full = mem;
    for (part, amount) in [(Part::Temp, 1), (Part::Buffers, usize::MAX)] {
        let error = mem.charge(part, amount).unwrap_err();
        assert_eq!(
            error.serialize(),
            MdsError::compile_memory_exceeded().serialize()
        );
        assert_eq!(mem, full);
    }
}

#[test]
fn nested_marks_release_temporaries_but_keep_escaped_buffers() {
    let mut mem = MemBudget::default();
    let outer = mem.mark();
    mem.charge(Part::Temp, 3).unwrap();
    let inner = mem.mark();
    mem.charge(Part::Buffers, 4).unwrap();
    mem.transfer(Part::Buffers, Part::Temp, 4);
    mem.transfer(Part::Temp, Part::Buffers, 1);
    mem.release_to(inner);
    assert_eq!(mem.mark(), inner);
    mem.release_to(outer);
    assert_eq!(mem.mark(), outer);
    mem.charge(Part::Buffers, MAX_COMPILE_MEMORY - 1).unwrap();
    assert!(mem.charge(Part::Temp, 1).is_err());
}

#[test]
fn each_invalid_release_latches_without_change_to_the_counts() {
    let releases: [fn(&mut MemBudget); 4] = [
        |mem| mem.transfer(Part::Buffers, Part::Temp, 1),
        |mem| mem.transfer(Part::Temp, Part::Buffers, 1),
        |mem| mem.release_to(1),
        |mem| mem.release_buffers(1),
    ];
    for release in releases {
        let mut mem = MemBudget::default();
        let empty = mem;
        release(&mut mem);
        mem.release_to(0);
        assert_eq!(
            (mem.buffers, mem.mark(), mem.headroom),
            (0, 0, empty.headroom)
        );
        assert_ne!(mem, empty, "the latch must survive a valid release");
    }
}

#[test]
fn refused_growth_leaves_the_buffer_unchanged() {
    let mut mem = MemBudget::default();
    let mut out = String::new();
    super::super::push_capped(&mut out, "abc", &mut mem).unwrap();
    let capacity = out.capacity();
    mem.charge(Part::Temp, MAX_COMPILE_MEMORY - capacity)
        .unwrap();
    let error = super::super::push_capped(&mut out, "def", &mut mem).unwrap_err();
    assert_eq!(
        error.serialize(),
        MdsError::compile_memory_exceeded().serialize()
    );
    assert_eq!(out, "abc");
    assert_eq!(out.capacity(), capacity);
}

#[test]
fn byte_size_does_not_wrap() {
    let count = usize::MAX / NODE;
    assert_eq!(bytes(count, NODE), Some(count * NODE));
    assert_eq!(bytes(count + 1, NODE), None);
}

#[test]
fn display_count_matches_nested_values_and_number_formats() {
    let value = Value::Array(vec![
        Value::Object(std::collections::HashMap::from([
            ("z".into(), Value::Boolean(false)),
            (
                "a".into(),
                Value::Array(vec![Value::Null, Value::Number(f64::MAX)]),
            ),
        ])),
        Value::Number(-0.0),
        Value::String("é".into()),
    ]);
    let len = display_len(&value).unwrap();
    let text = render_value(&value, len).unwrap();
    assert_eq!(text, value.to_string());
    assert_eq!(text.capacity(), text.len());
}

#[test]
fn value_count_checks_depth_and_charges_spare_array_capacity() {
    let mut items = Vec::with_capacity(16);
    items.push(Value::Null);
    assert_eq!(
        value_bytes(&Value::Array(items)),
        Some(VALUE_ROOT + 16 * NODE)
    );
    let mut value = Value::Null;
    for _ in 0..MAX_VALUE_DEPTH {
        value = Value::Array(vec![value]);
    }
    assert!(value_bytes(&value).is_some());
    let too_deep = Value::Array(vec![value]);
    assert_eq!(value_bytes(&too_deep), None);
    assert_eq!(display_len(&too_deep), None);
}
