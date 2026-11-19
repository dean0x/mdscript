use super::{MemBudget, Part};
use crate::{error::MdsError, limits::MAX_COMPILE_MEMORY};

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
fn invalid_releases_leave_the_counters_unchanged() {
    let mut mem = MemBudget::default();
    let empty = mem;
    mem.transfer(Part::Buffers, Part::Temp, 1);
    mem.transfer(Part::Temp, Part::Buffers, 1);
    mem.release_to(1);
    mem.release_buffers(1);
    assert_eq!(mem, empty);
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
