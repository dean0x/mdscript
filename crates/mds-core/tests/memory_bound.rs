//! This binary has one test. The process-wide allocator counts allocations from all threads.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use mds::{CompileOptions, CompileResult, MdsError, Value};

const MIB: usize = 1024 * 1024;
const CAP_PASS: usize = MIB + 1;
const SLACK: usize = 8 * MIB;

/// Live heap bytes, and the high-water mark since `peak_growth` last reset it.
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static LARGEST_REQUEST: AtomicUsize = AtomicUsize::new(0);

/// The system allocator. A `realloc` counts its net change toward the peak
/// and its full new size as a request.
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
            LARGEST_REQUEST.fetch_max(layout.size(), Ordering::SeqCst);
            grow(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc_zeroed`'s contract.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            LARGEST_REQUEST.fetch_max(layout.size(), Ordering::SeqCst);
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
            LARGEST_REQUEST.fetch_max(new_size, Ordering::SeqCst);
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

fn nested_function_source(frontmatter: &str, depth: usize) -> String {
    let mut source = frontmatter.to_owned();
    let body = "{{x}}\n".repeat(49);
    for i in 1..=depth {
        source.push_str(&format!("@define f{i}():\n"));
        source.push_str(&body);
        if i < depth {
            source.push_str(&format!("{{{{length(f{}())}}}}\n", i + 1));
        }
        source.push_str("@end\n");
    }
    if depth == 0 {
        source.push_str("{{length(x)}}\n");
    } else {
        source.push_str("{{length(f1())}}\n");
    }
    source
}

fn assert_nested_function_peak() {
    let x = "a".repeat(MIB - "x: \n".len());
    let frontmatter = format!("---\nx: {x}\n---\n");
    let (flat_result, flat) = compile_measured(nested_function_source(&frontmatter, 0));
    let cases = [2, 4, 5, 8].map(|depth| {
        let (result, peak) = compile_measured(nested_function_source(&frontmatter, depth));
        (depth, result, peak, peak.saturating_sub(flat))
    });
    let bound = 4 * 50 * MIB + CAP_PASS + SLACK;
    eprintln!("function peak heap (B): D=0={flat}; growth limit={bound}");
    for (depth, _, peak, growth) in &cases {
        eprintln!("function peak heap (B): D={depth} peak={peak} growth={growth}");
    }

    let flat_output = flat_result.unwrap().into_markdown().unwrap();
    assert!(flat_output == format!("{frontmatter}{}\n", x.len()));
    for (depth, result, _, growth) in cases {
        if depth == 2 {
            let lines_len = 49 * (x.len() + 1);
            let inner_len = lines_len - 1;
            let outer_len = lines_len + inner_len.to_string().len();
            let output = result.unwrap().into_markdown().unwrap();
            assert!(output == format!("{frontmatter}{outer_len}\n"));
            continue;
        }
        assert!(
            growth <= bound,
            "D={depth} adds {growth} B (limit: {bound} B)"
        );
        if depth > 4 {
            let error = result.expect_err("compile memory limit error").serialize();
            assert_eq!(error.code, "mds::resource_limit", "D={depth}");
            assert_eq!(
                error.message,
                "resource limit exceeded: compile memory exceeds maximum of 209715200 bytes",
                "D={depth}"
            );
            assert!(error.help.is_none(), "D={depth}");
            assert!(error.span.is_none(), "D={depth}");
        }
    }
}

fn assert_message_edge_trim_peak() {
    let payload = format!("\u{2003}{}\r\nb  ", "a".repeat(8 * MIB));
    let payload_len = payload.len();
    // Keep room for one runtime variable copy and the two passages in the output.
    // A nonempty prefix leaves room for the final newline after the buffer doubles.
    let bound = 3 * payload_len + MIB;
    let expected = format!("{payload}{payload}");
    let cases = [
        (false, "@message system:\n\u{2003}{{payload}}{{payload}}\n@end\n"),
        (true, "@define role():\n\u{2003}{{payload}}{{payload}}\n@end\n@message {{role()}}:\nok\n@end\n"),
    ];
    let peaks = cases.map(|(check_role, source)| {
        let vars = HashMap::from([("payload".to_owned(), Value::String(payload.clone()))]);
        let (result, peak) = peak_growth(|| mds::compile_str_with(source, None, Some(vars)));
        let messages = result.unwrap().into_messages().unwrap();
        assert_eq!(messages.len(), 1);
        let actual = if check_role {
            &messages[0].role
        } else {
            &messages[0].content
        };
        assert!(
            actual == expected.trim(),
            "check_role={check_role}, length {}, first bytes {:?}, last bytes {:?}",
            actual.len(),
            actual.as_bytes().get(..8),
            actual.as_bytes().get(actual.len().saturating_sub(8)..)
        );
        assert!(peak >= payload_len, "one-buffer control: {peak} B");
        peak
    });
    eprintln!("message content and function role peaks: {peaks:?} B; limit: {bound} B");
    assert!(peaks.iter().all(|&peak| peak <= bound));
}

fn assert_small_messages_do_not_keep_padding() {
    let vars = HashMap::from([("pad".to_owned(), Value::String(" ".repeat(4 * MIB)))]);
    let source = "@message system:\n{{pad}}ok\n@end\n".repeat(6);
    let (result, peak) = peak_growth(|| mds::compile_str_with(&source, None, Some(vars)));
    let messages = result.unwrap().into_messages().unwrap();
    assert_eq!(messages.len(), 6);
    assert!(messages.iter().all(|message| message.content == "ok"));
    assert!(peak <= 17 * MIB, "padded messages kept {peak} B at peak");
}

fn assert_small_markdown_does_not_keep_padding() {
    let frontmatter = "---\nlabel: value\n---\n";
    let source = format!("{frontmatter}{{{{body}}}}");
    let padded = format!("ok{}", " ".repeat(64 * 1024));
    let vars = HashMap::from([("body".to_owned(), Value::String(padded))]);
    let output = mds::compile_str_with(&source, None, Some(vars))
        .unwrap()
        .into_markdown()
        .unwrap();
    assert_eq!(output, format!("{frontmatter}ok\n"));
    assert!(
        output.capacity() <= 2 * output.len(),
        "{} B",
        output.capacity()
    );
}

fn assert_mapped_output_peak() {
    let source = "---\ntype: mds\nlabel: value\n---\n{{body}}";
    let vars = HashMap::from([(
        "body".to_owned(),
        Value::String(format!("{}\rb", "a".repeat(8 * MIB - 2))),
    )]);
    let modules = HashMap::from([("main.mds".to_owned(), source.to_owned())]);
    let opts = CompileOptions::default()
        .with_source_map(true)
        .with_include_sources_content(true);
    let (result, peak) =
        peak_growth(|| mds::compile_virtual_with_deps_opts(modules, "main.mds", Some(vars), opts));
    let result = result.unwrap();
    let map = result.source_map.as_ref().unwrap();
    assert_eq!(
        map.sources_content.as_deref(),
        Some([source.to_owned()].as_slice())
    );
    assert!(!map.mappings.is_empty());
    let output = result.into_markdown().unwrap();
    assert_eq!(output.len(), "---\nlabel: value\n---\n".len() + 8 * MIB);
    assert!(output.starts_with("---\nlabel: value\n---\na"));
    assert!(output.ends_with("ab\n"));
    assert!(!output.contains('\r'));
    assert!(peak >= 8 * MIB, "output control: {peak} B");
    // Allow the runtime value, one output buffer, and 4 MiB of other data.
    let bound = 20 * MIB;
    eprintln!("mapped output peak: {peak} B; limit: {bound} B");
    assert!(peak <= bound, "mapped output exceeds peak limit");
}

fn assert_over_cap_upper_does_not_build_its_result() {
    let cap = 50 * MIB;
    // Each U+0390 uses two bytes and uppercases to six bytes.
    let payload = "\u{0390}".repeat(cap / 6 + 1);
    let args_bytes = 2 * payload.len();
    let vars = HashMap::from([("payload".to_owned(), Value::String(payload))]);
    let (result, peak) =
        peak_growth(|| mds::compile_str_with("{{upper(payload)}}", None, Some(vars)));
    eprintln!("over-cap upper peak: {peak} B; limit: {cap} B");
    assert!(peak >= args_bytes, "argument copies: {peak} B");
    assert!(peak < cap, "over-cap upper grew peak heap by {peak} B");
    let error = result.expect_err("upper output cap error");
    assert_eq!(error.serialize().code, "mds::builtin");
    assert!(error
        .to_string()
        .ends_with("upper() output exceeds maximum size of 52428800 bytes"));
}

fn assert_over_cap_lower_with_sigma_does_not_build_its_result() {
    let cap = 50 * MIB;
    // Each İ maps to three bytes. Σa adds three more bytes for cap + 1.
    let mut payload = "İ".repeat(cap / 3);
    payload.push_str("Σa");
    LARGEST_REQUEST.store(0, Ordering::SeqCst);
    let eager = payload.to_lowercase();
    let eager_request = LARGEST_REQUEST.load(Ordering::SeqCst);
    assert_eq!(eager.len(), cap + 1);
    assert!(
        eager_request > cap,
        "eager lower control: {eager_request} B"
    );
    drop(eager);

    let vars = HashMap::from([("payload".to_owned(), Value::String(payload))]);
    LARGEST_REQUEST.store(0, Ordering::SeqCst);
    let result = mds::compile_str_with("{{lower(payload)}}", None, Some(vars));
    let request = LARGEST_REQUEST.load(Ordering::SeqCst);
    eprintln!("over-cap lower largest request: {request} B; eager control: {eager_request} B");
    assert!(request < cap, "over-cap lower requested {request} B");
    let error = result.expect_err("lower output cap error").serialize();
    assert_eq!(error.code, "mds::builtin");
    assert_eq!(
        error.message,
        "lower() output exceeds maximum size of 52428800 bytes"
    );
}

fn assert_join_refusal_precedes_result_allocation() {
    let measured = |count| {
        let vars = HashMap::from([
            (
                "parts".to_owned(),
                Value::Array(vec![Value::String(String::new()); count]),
            ),
            ("sep".to_owned(), Value::String("x".repeat(MIB))),
        ]);
        LARGEST_REQUEST.store(0, Ordering::SeqCst);
        let result = mds::compile_str_with("{{length(join(parts, sep))}}", None, Some(vars));
        (result, LARGEST_REQUEST.load(Ordering::SeqCst))
    };
    let (fits, control) = measured(5);
    assert_eq!(fits.unwrap().into_markdown().unwrap(), "4194304\n");
    assert!(control >= 4 * MIB, "join control: {control} B");

    let (refused, largest) = measured(52);
    let error = refused.expect_err("join output cap error").serialize();
    assert_eq!(error.code, "mds::builtin");
    assert_eq!(
        error.message,
        "join() output exceeds maximum size of 52428800 bytes"
    );
    assert!(largest < 4 * MIB, "refused join requested {largest} B");
}

fn argument_source(frontmatter: &str, term: &str, count: usize) -> String {
    let params = (0..count).map(|i| format!("p{i}")).collect::<Vec<_>>();
    let args = vec![term; count].join(", ");
    format!(
        "{frontmatter}@define sink({}):\nok\n@end\n{{{{sink({args})}}}}\n",
        params.join(", ")
    )
}

fn iterable_source(frontmatter: &str, iterable: &str) -> String {
    let mut source = frontmatter.to_owned();
    for i in 0..6 {
        source.push_str(&format!("@define f{i}():\n@for item in {iterable}:\n"));
        if i == 5 {
            source.push_str("ok\n");
        } else {
            source.push_str(&format!("{{{{f{}()}}}}\n", i + 1));
        }
        source.push_str("@end\n@end\n");
    }
    source.push_str("{{f0()}}\n");
    source
}

fn assert_value_peaks() {
    let text = "a".repeat(MIB - 64);
    let chunk = format!(
        "---\nsmall: [a]\nfirst_item: {text}\n---\n@define chunk():\n{}@end\n",
        "{{first_item}}\n".repeat(48)
    );
    let copying_frontmatter = format!(
        "---\nitem: &part {}\nitems: [{}]\n---\n",
        "a".repeat(MIB / 2),
        vec!["*part"; 16].join(", ")
    );
    let expanded = format!(
        "replace(first_item, '{}', '{}')",
        "a".repeat(64),
        "b".repeat(3072)
    );
    let cases = [
        (
            "computed arguments",
            argument_source(&chunk, &expanded, 6),
            argument_source(&chunk, "first_item", 6),
        ),
        (
            "computed iterables",
            iterable_source(&chunk, "split(chunk(), '|')"),
            iterable_source(&chunk, "small"),
        ),
        (
            "array copies",
            argument_source(&copying_frontmatter, "reverse(items)", 34),
            argument_source(&copying_frontmatter, "items", 34),
        ),
    ];
    let outcomes = cases.map(|(label, source, control)| {
        let (control, baseline) = compile_measured(control);
        let (result, peak) = compile_measured(source);
        let growth = peak.saturating_sub(baseline);
        eprintln!("{label} heap (B): control={baseline}, peak={peak}, growth={growth}");
        (label, control, result, growth)
    });
    for (label, control, result, growth) in outcomes {
        assert!(control
            .unwrap()
            .into_markdown()
            .unwrap()
            .ends_with("\nok\n"));
        let limit = 200 * MIB + SLACK;
        assert!(
            growth <= limit,
            "{label} adds {growth} B (limit: {limit} B)"
        );
        let error = result.expect_err(label).serialize();
        assert_eq!(error.code, "mds::resource_limit", "{label}");
        assert_eq!(
            error.message,
            "resource limit exceeded: compile memory exceeds maximum of 209715200 bytes",
            "{label}"
        );
    }
}

#[test]
fn compile_peak_heap_stays_within_each_bound() {
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

    assert_nested_function_peak();
    assert_message_edge_trim_peak();
    assert_small_messages_do_not_keep_padding();
    assert_small_markdown_does_not_keep_padding();
    assert_mapped_output_peak();
    assert_join_refusal_precedes_result_allocation();
    assert_over_cap_upper_does_not_build_its_result();
    assert_over_cap_lower_with_sigma_does_not_build_its_result();
    assert_value_peaks();
}
