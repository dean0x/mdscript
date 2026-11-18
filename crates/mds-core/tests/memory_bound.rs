//! This binary has one test. The process-wide allocator counts allocations from all threads.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use mds::{CompileResult, MdsError, Value};

const MIB: usize = 1024 * 1024;
const CAP_PASS: usize = MIB + 1;
const SLACK: usize = 8 * MIB;

/// Live heap bytes, and the high-water mark since `peak_growth` last reset it.
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// The system allocator, counting live bytes. A `realloc` counts as its net change
/// (`new - old`), not as a fresh allocation plus a free.
struct Counting;

fn grow(bytes: usize) {
    let now = CURRENT.fetch_add(bytes, Ordering::SeqCst) + bytes;
    PEAK.fetch_max(now, Ordering::SeqCst);
}

fn shrink(bytes: usize) {
    CURRENT.fetch_sub(bytes, Ordering::SeqCst);
}

// SAFETY: every method forwards to `System` with the caller's arguments unchanged and
// only adjusts the counters, so `System`'s guarantees hold for every returned pointer.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc`'s contract.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc_zeroed`'s contract.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged; `ptr` was allocated by `System` via this type.
        unsafe { System.dealloc(ptr, layout) };
        shrink(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `realloc`'s contract.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            if new_size >= layout.size() {
                grow(new_size - layout.size());
            } else {
                shrink(layout.size() - new_size);
            }
        }
        new_ptr
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Run `f`, returning its result and the peak heap growth above the live bytes at the
/// moment it started. Fixtures are built before the call, so they are not counted.
fn peak_growth<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let start = CURRENT.load(Ordering::SeqCst);
    PEAK.store(start, Ordering::SeqCst);
    let out = f();
    (out, PEAK.load(Ordering::SeqCst) - start)
}

fn test_count(source: &str) -> usize {
    source
        .lines()
        .filter(|line| line.trim() == "#[test]")
        .count()
}

fn compile_measured(source: String) -> (Result<CompileResult, MdsError>, usize) {
    let modules = HashMap::from([("main.mds".to_string(), source)]);
    peak_growth(move || mds::compile_virtual(modules, "main.mds", None))
}

fn nested_if_source(depth: usize) -> String {
    // The YAML key and newline share the 1 MiB frontmatter cap with the scalar.
    let x = "a".repeat(MIB - "x: \n".len());
    let mut source = format!("---\nx: {x}\n---\n");
    if depth == 0 {
        // Fifty lines fit because the scalar is a few bytes short of 1 MiB.
        source.push_str(&"{{x}}\n".repeat(51));
    } else {
        let body = "{{x}}\n".repeat(49);
        for _ in 0..depth {
            source.push_str("@if x:\n");
            source.push_str(&body);
        }
        source.push_str(&"@end\n".repeat(depth));
    }
    source
}

#[test]
fn nested_if_output_does_not_raise_the_peak_heap() {
    let source = include_str!("memory_bound.rs");
    assert_eq!(test_count(source), 1);
    assert_eq!(
        test_count(&format!("{source}\n#[test]\nfn extra() {{}}\n")),
        2
    );

    // Supply the text through a variable to stay below the source file cap.
    let modules = HashMap::from([("main.mds".to_string(), "{{body}}".to_string())]);
    let vars = HashMap::from([("body".to_string(), Value::String("a".repeat(40 * MIB)))]);
    let (control, control_peak) =
        peak_growth(move || mds::compile_virtual(modules, "main.mds", Some(vars)));
    assert_eq!(
        control.unwrap().into_markdown().unwrap().len(),
        40 * MIB + 1
    );
    assert!(control_peak >= 40 * MIB, "control peak: {control_peak} B");

    let (flat_result, flat) = compile_measured(nested_if_source(0));
    let (nested_result, nested) = compile_measured(nested_if_source(4));
    let growth = nested.saturating_sub(flat);
    eprintln!("peak heap (B): control={control_peak}, D=0={flat}, D=4={nested}, growth={growth}");
    let message = "resource limit exceeded: output exceeds maximum size of 52428800 bytes";
    for (depth, result) in [(0, flat_result), (4, nested_result)] {
        assert_eq!(
            result.expect_err("output cap error").to_string(),
            message,
            "D={depth}"
        );
    }
    assert!(
        growth <= CAP_PASS + SLACK,
        "D=4 adds {growth} B to the peak (limit: {} B)",
        CAP_PASS + SLACK
    );
}
