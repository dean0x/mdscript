//! This binary has one test because it uses a process-wide allocator.
//! A second test would add its allocations to the peak heap measure.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};

const MIB: usize = 1024 * 1024;

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

fn define_peak(k: usize) -> usize {
    let mut source = String::new();
    for i in 1..=k {
        writeln!(source, "@define f{i}(x):\nv{{{{x}}}}\n@end").unwrap();
    }
    write!(source, "{{{{f{k}(\"a\")}}}}").unwrap();

    let (result, peak) = peak_growth(|| mds::compile_str(&source));
    assert_eq!(result.unwrap().into_markdown().unwrap(), "va\n", "K={k}");
    peak
}

#[test]
fn peak_heap_growth_stays_below_one_mib_for_four_additional_defines() {
    let source = include_str!("define_growth.rs");
    assert_eq!(test_count(source), 1);
    assert_eq!(test_count(include_str!("alloc_bounds.rs")), 1);
    let extra_test = format!("{source}\n#[test]\nfn extra() {{}}\n");
    assert_eq!(test_count(&extra_test), 2);
    assert!(source.lines().next().is_some_and(|line| {
        line.starts_with("//!") && line.contains("process-wide allocator")
    }));

    let peak_12 = define_peak(12);
    let peak_16 = define_peak(16);
    let increase = peak_16.saturating_sub(peak_12);

    // Check the control before the growth assertion so both run on the baseline.
    let body = "p".repeat(4 * MIB);
    let (result, control_peak) = peak_growth(|| mds::compile_str(&body));
    let output = result.unwrap().into_markdown().unwrap();
    assert!(output.strip_suffix('\n') == Some(body.as_str()));
    assert!(control_peak >= body.len(), "control peak: {control_peak} B");
    assert!(
        increase < MIB,
        "Peak heap growth: K=12: {peak_12} B; K=16: {peak_16} B; increase: {increase} B; control: {control_peak} B"
    );
}
