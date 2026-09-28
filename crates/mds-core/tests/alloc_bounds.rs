//! Peak-heap bounds (#415): building output that crosses `MAX_OUTPUT_SIZE` must not
//! allocate far past it.
//!
//! The output cap is enforced before each append, and a capped buffer never reserves
//! past the cap, so a loop that crosses it holds at most the cap plus one pass. When
//! the cap was checked only after a node had finished, a loop spent all of its memory
//! first — the error text is identical either way, so this measures the resource the
//! fix moves (applies PF-013): peak heap growth, under a counting global allocator.
//! `replace()` likewise computes its result's length before allocating it, so an
//! over-cap result is refused without being built.
//!
//! This binary holds exactly ONE `#[test]`: the allocator is process-wide, and a second
//! test running on another thread would add its allocations to the measurement.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use mds::{MdsError, Value};

/// Mirrors the private `limits::MAX_OUTPUT_SIZE` (50 MiB). The error assertions
/// require the limit message, which prints the real value, so a drift fails them.
const MAX_OUTPUT_SIZE: usize = 50 * 1024 * 1024;

/// Allowance over the cap for everything a pass holds besides the capped buffer: the
/// pass's own rendered text, the interpolated value's copy, and the scope's copy of
/// the runtime variables.
const SLACK: usize = 8 * 1024 * 1024;

/// One loop pass renders `{{x}}\n`: `x` is 1 MiB, plus the newline.
const CAP_PASS: usize = 1024 * 1024 + 1;

/// The pass that crosses the cap: 49 passes fit, the 50th does not.
const CAP_CROSSING_PASS: usize = MAX_OUTPUT_SIZE / CAP_PASS + 1;

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

fn numbers(n: usize) -> Value {
    Value::Array((0..n).map(|i| Value::Number(i as f64)).collect())
}

/// Compile `source` as `main.mds` with runtime `vars`, measuring its peak heap growth.
fn compile_measured(
    source: &str,
    vars: HashMap<String, Value>,
) -> (Result<mds::CompileResult, MdsError>, usize) {
    let modules = HashMap::from([("main.mds".to_string(), source.to_string())]);
    peak_growth(move || mds::compile_virtual(modules, "main.mds", Some(vars)))
}

/// The message of `result`'s error, reported by kind only on success.
fn error_message<T>(result: Result<T, MdsError>, context: &str) -> String {
    match result {
        Ok(_) => panic!("{context}: compiled successfully, expected an error"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn peak_heap_growth_stays_within_the_output_cap() {
    // ── Loops (#415): a crossing loop holds at most the cap plus one pass ─────────
    //
    // A small array with a large pass body, so the loop's own copy of the array stays
    // small and the growth measured is the output being built.
    let loop_cases = [
        ("top-level @for", "@for i in items:\n{{x}}\n@end\n"),
        (
            "@for in @define",
            "@define f():\n@for i in items:\n{{x}}\n@end\n@end\n{{f()}}\n",
        ),
    ];
    // Every case is measured before anything is asserted, so a failure reports them all.
    let measured: Vec<(&str, String, usize)> = loop_cases
        .iter()
        .map(|&(case, source)| {
            let vars = HashMap::from([
                ("x".to_string(), Value::String("a".repeat(CAP_PASS - 1))),
                ("items".to_string(), numbers(2 * CAP_CROSSING_PASS)),
            ]);
            let (result, peak) = compile_measured(source, vars);
            (case, error_message(result, case), peak)
        })
        .collect();
    let report = format!("(case, error, peak heap growth in bytes): {measured:#?}");
    eprintln!("{report}");
    for (case, message, peak) in &measured {
        assert_eq!(
            *message,
            format!(
                "resource limit exceeded: output exceeds maximum size of {MAX_OUTPUT_SIZE} bytes"
            ),
            "{case}"
        );
        // Positive control: the allocator saw the loop build its output up to the pass
        // before the crossing one — a counter that missed allocations would pass the
        // bound below vacuously.
        assert!(
            *peak >= (CAP_CROSSING_PASS - 1) * CAP_PASS,
            "{case}: peak heap growth is below the {} B the loop must have built; {report}",
            (CAP_CROSSING_PASS - 1) * CAP_PASS
        );
        assert!(
            *peak <= MAX_OUTPUT_SIZE + SLACK,
            "{case}: peak heap growth exceeds the cap plus one pass ({} B); {report}",
            MAX_OUTPUT_SIZE + SLACK
        );
    }

    // ── replace() (#415): an over-cap result is refused before it is allocated ────
    //
    // `s` holds one single-byte match per MiB of result and `to` is 1 MiB, so 300
    // matches ask for a ~300 MiB result. The control's 40 matches give a 40 MiB result
    // that fits, so the allocator must see `replace()` build it.
    let to = "a".repeat(1024 * 1024);
    let replace_measured: Vec<(&str, Result<usize, String>, usize)> =
        [("over the cap", 300), ("control: under the cap", 40)]
            .iter()
            .map(|&(case, matches)| {
                let vars = HashMap::from([
                    ("s".to_string(), Value::String("x".repeat(matches))),
                    ("to".to_string(), Value::String(to.clone())),
                ]);
                let (result, peak) = compile_measured("{{replace(s, \"x\", to)}}\n", vars);
                let outcome = result
                    .and_then(mds::CompileResult::into_markdown)
                    .map(|text| text.len())
                    .map_err(|err| err.to_string());
                (case, outcome, peak)
            })
            .collect();
    let report =
        format!("(case, output length or error, peak heap growth in bytes): {replace_measured:#?}");
    eprintln!("{report}");
    let [(_, over, over_peak), (_, control, control_peak)] = &replace_measured[..] else {
        unreachable!("two replace() cases");
    };
    assert_eq!(
        *over,
        Err(format!(
            "replace() output exceeds maximum size of {MAX_OUTPUT_SIZE} bytes"
        )),
        "an over-cap replace() must fail with the built-in's own error; {report}"
    );
    assert!(
        *over_peak < SLACK,
        "an over-cap replace() must be refused before its result is allocated; {report}"
    );
    // Positive control: a result that fits is built (the output is the result and the
    // template's newline), and the allocator sees it.
    assert_eq!(*control, Ok(40 * 1024 * 1024 + 1), "{report}");
    assert!(
        *control_peak >= 40 * 1024 * 1024,
        "the allocator must see the control's 40 MiB result; {report}"
    );
}
