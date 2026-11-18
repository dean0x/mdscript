//! This binary has one test because it uses a process-wide allocator.
//! A second test would add its allocations to the peak heap measure.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};

use mds::Value;

const ITEM_SIZE: usize = 16_384;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

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
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);

            grow(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc_zeroed`'s contract.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);

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
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);

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

fn loop_peak(dot: bool, depth: usize, through_calls: bool) -> usize {
    let items = Value::Array(vec![Value::Array(vec![Value::Null; ITEM_SIZE]); 2]);
    let (root, path, value) = if dot {
        (
            "cfg",
            "cfg.items",
            Value::Object(HashMap::from([("items".into(), items)])),
        )
    } else {
        ("items", "items", items)
    };
    let vars = HashMap::from([(root.into(), value)]);
    let mut source = String::new();
    let iterable = if through_calls {
        source.push_str("@define f0(arg):\n");
        "arg"
    } else {
        path
    };
    for i in 0..depth {
        writeln!(source, "@for item{i} in {iterable}:").unwrap();
    }
    source.push_str("ok\n");
    for _ in 0..depth {
        source.push_str("@end\n");
    }
    if through_calls {
        source.push_str("@end\n");
        for i in 1..4 {
            writeln!(source, "@define f{i}(arg):\n{{{{f{}(arg)}}}}\n@end", i - 1).unwrap();
        }
        writeln!(source, "{{{{f3({path})}}}}").unwrap();
    }
    let (result, peak) = peak_growth(|| mds::compile_str_with(&source, None, Some(vars)));
    let output = result.unwrap().into_markdown().unwrap();
    assert_eq!(output, "ok\n".repeat(1 << depth));
    peak
}

fn call_allocations(large: bool, calls: usize) -> usize {
    let vars = (0..if large { 2_000 } else { 1 })
        .map(|i| (format!("v{i}"), Value::Null))
        .collect();
    let mut source = String::new();
    if large {
        for i in 0..200 {
            writeln!(source, "@define prior{i}():\nunused\n@end").unwrap();
        }
    }
    source.push_str("@define target():\nok\n@end\n");
    source.push_str(&"{{target()}}\n".repeat(calls));
    let start = ALLOCATIONS.load(Ordering::Relaxed);
    let result = mds::compile_str_with(&source, None, Some(vars));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - start;
    assert_eq!(
        result.unwrap().into_markdown().unwrap(),
        "ok\n".repeat(calls)
    );
    allocations
}

#[test]
fn shared_reads_keep_heap_growth_independent_of_value_and_capture_size() {
    let source = include_str!("shared_reads.rs");
    assert_eq!(test_count(source), 1);
    assert_eq!(
        test_count(&format!("{source}\n#[test]\nfn extra() {{}}\n")),
        2
    );

    let array_bytes = (2 * ITEM_SIZE + 2) * std::mem::size_of::<Value>();
    let peaks = [false, true].map(|dot| {
        [
            loop_peak(dot, 1, false),
            loop_peak(dot, 8, false),
            loop_peak(dot, 1, true),
            loop_peak(dot, 8, true),
        ]
    });
    for [one, ..] in peaks {
        assert!(
            one >= array_bytes,
            "one loop peak {one} B < {array_bytes} B"
        );
    }
    let extra =
        [false, true].map(|large| call_allocations(large, 1_000) - call_allocations(large, 10));
    assert!(extra[0] >= 990, "small capture control: {extra:?}");
    assert!(
        extra[1] <= extra[0] + 990,
        "extra calls, small and large captures: {extra:?}"
    );
    assert!(
        peaks.iter().all(|&[one, eight, call_one, call_eight]| {
            [eight, call_one, call_eight].iter().all(|peak| peak.saturating_sub(one) < array_bytes / 2)
        }),
        "bare and dot peaks [one, eight, call one, call eight]: {peaks:?} B; allowed growth < {} B; extra calls: {extra:?}",
        array_bytes / 2,
    );
}
