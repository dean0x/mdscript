//! This binary has one test. The process-wide allocator counts allocations from all threads.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use mds::{CompileOptions, CompileResult, MdsError, Value};

const MIB: usize = 1024 * 1024;
const CAP_PASS: usize = MIB + 1;
const SLACK: usize = 8 * MIB;
/// Peak heap bytes per source byte on a 64-bit host while one module with dense syntax
/// resolves: its tokens, its syntax tree and the copy of a function or block body. The
/// compile memory budget does not count them; spec.md and SECURITY.md give this number.
const TREE_PEAK_PER_BYTE: usize = 432;
/// The peak while the lexer runs: its tokens and its arrays of characters and offsets.
const LEXER_PEAK_PER_BYTE: usize = 56;
/// Heap bytes per source byte that a cached module with a dense function body keeps
/// until the compile returns.
const TREE_KEPT_PER_BYTE: usize = 388;

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

/// Frontmatter whose `items` holds `[a]`, `[a, a]` and so on, up to `count` copies of
/// one `len`-byte scalar, then `{{length(call)}}`. Each item displays differently, so
/// `unique()` keeps every item, and a small source expands into a large array.
fn distinct_items_source(len: usize, count: usize, call: &str) -> String {
    let items = (1..=count)
        .map(|n| format!("[{}]", vec!["*a"; n].join(", ")))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "---\na: &a {}\nitems: [{items}]\n---\n{{{{length({call})}}}}\n",
        "x".repeat(len)
    )
}

fn assert_unique_is_charged_before_it_copies() {
    const COUNT: usize = 26;
    // The array holds the scalar 351 times.
    let copies = COUNT * (COUNT + 1) / 2;
    let measured = |len, call| compile_measured(distinct_items_source(len, COUNT, call));
    let counts_items = |result: Result<CompileResult, MdsError>| {
        result.unwrap().into_markdown().unwrap().ends_with("\n26\n")
    };

    // It fits: unique() builds its keys and its result. Its charge is the larger of
    // two sums: two array copies and 32 bytes per item, or one array copy, the key
    // bytes and the seen table. The checks below bound the heap growth of unique(),
    // not its charge.
    let len = 64 * 1024;
    let text = copies * len;
    let (control, baseline) = measured(len, "items");
    let (fits, peak) = measured(len, "unique(items)");
    let growth = peak.saturating_sub(baseline);
    eprintln!("unique heap (B): control={baseline}, peak={peak}, growth={growth}, text={text}");
    assert!(counts_items(control));
    assert!(counts_items(fits));
    assert!(growth >= text, "unique() copies: {growth} B");
    assert!(growth <= 2 * text + MIB, "unique() grows {growth} B");

    // The array copy fits, but the bound does not: unique() fails before it copies.
    let len = 128 * 1024;
    let text = copies * len;
    let (control, baseline) = measured(len, "items");
    let (refused, peak) = measured(len, "unique(items)");
    let growth = peak.saturating_sub(baseline);
    eprintln!("refused unique heap (B): control={baseline}, peak={peak}, growth={growth}");
    assert!(counts_items(control));
    // A success is not printed: its output is large.
    let Err(error) = refused else {
        panic!("unique() must fail with the compile memory error");
    };
    let error = error.serialize();
    assert_eq!(error.code, "mds::resource_limit");
    assert_eq!(
        error.message,
        "resource limit exceeded: compile memory exceeds maximum of 209715200 bytes"
    );
    assert!(growth < text / 2, "refused unique() grows {growth} B");
}

fn assert_message_role_peak() {
    const COUNT: usize = 300;
    let text = "A".repeat(MIB - 64);
    let frontmatter = format!("---\nx: {text}\nsmall: A\n---\n");
    let source = |name| {
        let body = format!("@message {{{{lower({name})}}}}:\nb\n@end\n");
        format!("{frontmatter}{}", body.repeat(COUNT))
    };
    let (control, baseline) = compile_measured(source("small"));
    let messages = control.unwrap().into_messages().unwrap();
    assert_eq!(messages.len(), COUNT);
    assert!(messages.iter().all(|m| m.role == "a" && m.content == "b"));

    let (result, peak) = compile_measured(source("x"));
    let growth = peak.saturating_sub(baseline);
    eprintln!("message role heap (B): control={baseline}, peak={peak}, growth={growth}");
    let limit = 200 * MIB + SLACK;
    assert!(
        growth <= limit,
        "message roles add {growth} B (limit: {limit} B)"
    );
    let error = result.expect_err("message role budget").serialize();
    assert_eq!(error.code, "mds::resource_limit");
    assert_eq!(
        error.message,
        "resource limit exceeded: compile memory exceeds maximum of 209715200 bytes"
    );
}

fn import_peak(count: usize) -> (Result<CompileResult, MdsError>, usize) {
    let mut root = String::new();
    let mut modules = HashMap::new();
    for i in 0..count {
        root.push_str(&format!("@import \"./part{i}.mds\" as part{i}\n"));
        let unused = "p".repeat(MIB);
        modules.insert(
            format!("part{i}.mds"),
            format!("@define unused():\n{unused}\n@end\n{{{{chunk}}}}"),
        );
    }
    modules.insert("main.mds".to_owned(), root + "root");
    // Five prompts take 197.5 MiB. Their origins take more than 5 MiB.
    let vars = HashMap::from([(
        "chunk".to_owned(),
        Value::String("x".repeat(39 * MIB + MIB / 2)),
    )]);
    peak_growth(|| mds::compile_virtual(modules, "main.mds", Some(vars)))
}

fn assert_imported_prompts_share_one_limit() {
    let (control, baseline) = import_peak(4);
    assert_eq!(control.unwrap().into_markdown().unwrap(), "root\n");

    let (result, peak) = import_peak(5);
    eprintln!("retained prompt heap (B): control={baseline}, peak={peak}");
    let error = result.expect_err("retained prompt budget").serialize();
    assert_eq!(error.code, "mds::resource_limit");
    assert_eq!(
        error.message,
        "resource limit exceeded: compile memory exceeds maximum of 209715200 bytes"
    );
}

/// Four imported modules whose frontmatter repeats one scalar of `len` bytes through
/// 63 aliases, so a small source parses into a large tree. Each module defines a
/// function, which can capture the scope, so the module keeps its parsed tree and
/// its converted scope layer.
fn alias_frontmatter_peak(len: usize) -> (Result<CompileResult, MdsError>, usize) {
    let refs = vec!["*a"; 63].join(", ");
    let frontmatter = format!("---\na: &a {}\nb: [{refs}]\n---\n", "x".repeat(len));
    let mut root = String::new();
    let mut modules = HashMap::new();
    for i in 0..4 {
        root.push_str(&format!("@import \"./part{i}.mds\" as part{i}\n"));
        modules.insert(
            format!("part{i}.mds"),
            format!("{frontmatter}@define f():\nok\n@end\n"),
        );
    }
    modules.insert("main.mds".to_owned(), root + "root");
    peak_growth(|| mds::compile_virtual(modules, "main.mds", None))
}

fn assert_alias_frontmatter_shares_one_limit() {
    let (control, baseline) = alias_frontmatter_peak(1);
    assert_eq!(control.unwrap().into_markdown().unwrap(), "root\n");

    // Each module parses 64 copies of the scalar: 40 MiB from a 640 KiB source.
    let (result, peak) = alias_frontmatter_peak(5 * MIB / 8);
    let growth = peak.saturating_sub(baseline);
    eprintln!("alias frontmatter heap (B): control={baseline}, peak={peak}, growth={growth}");
    let limit = 200 * MIB + SLACK;
    assert!(
        growth <= limit,
        "alias frontmatter adds {growth} B (limit: {limit} B)"
    );
    let error = result.expect_err("alias frontmatter budget").serialize();
    assert_eq!(error.code, "mds::resource_limit");
    assert_eq!(
        error.message,
        "resource limit exceeded: compile memory exceeds maximum of 209715200 bytes"
    );
}

/// An `@extends` chain of 24 files. Each file adds a distinct key with a scalar of
/// `len` bytes, so each base keeps the merged tree of itself and every file above it.
fn extends_chain_peak(len: usize) -> (Result<CompileResult, MdsError>, usize) {
    let value = "a".repeat(len);
    let mut modules = HashMap::from([(
        "f0.mds".to_owned(),
        format!("---\nk0: {value}\n---\n@block main:\nok\n@end\n"),
    )]);
    for i in 1..24 {
        modules.insert(
            format!("f{i}.mds"),
            format!("---\nk{i}: {value}\n---\n@extends \"./f{}.mds\"\n", i - 1),
        );
    }
    peak_growth(|| mds::compile_virtual(modules, "f23.mds", None))
}

fn assert_extends_chain_trees_share_one_limit() {
    let (control, baseline) = extends_chain_peak(1);
    let output = control.unwrap().into_markdown().unwrap();
    assert!(output.starts_with("---\nk0: a\nk1: a\n"), "{output}");
    assert!(output.ends_with("k23: a\n---\nok\n"), "{output}");

    // The merged trees of the bases take about 276 MiB.
    let (result, peak) = extends_chain_peak(MIB - 8);
    let growth = peak.saturating_sub(baseline);
    eprintln!("extends chain heap (B): control={baseline}, peak={peak}, growth={growth}");
    // Each file still open holds two uncharged copies of its frontmatter text: its
    // tokens and its syntax tree, which are excluded stages.
    let limit = 200 * MIB + 24 * 2 * MIB + SLACK;
    assert!(
        growth <= limit,
        "extends chain adds {growth} B (limit: {limit} B)"
    );
    let error = result.expect_err("extends chain budget").serialize();
    assert_eq!(error.code, "mds::resource_limit");
    assert_eq!(
        error.message,
        "resource limit exceeded: compile memory exceeds maximum of 209715200 bytes"
    );
}

fn mapped_corner(extra: usize) -> (Result<CompileResult, MdsError>, usize) {
    let raw = format!("label: {}\n", "a".repeat(MIB - "label: \n".len()));
    let mut root = format!("---\n{raw}---\n");
    let mut modules = HashMap::new();
    for i in 0..5 {
        root.push_str(&format!(
            "@import \"./part{i}.mds\" as part{i}\n@include part{i}\n"
        ));
        let unused = "p".repeat(9 * MIB + 3 * MIB / 4);
        modules.insert(
            format!("part{i}.mds"),
            format!("@define unused():\n{unused}\n@end\nx\n"),
        );
    }
    modules.insert("main.mds".to_owned(), root + "{{body}}");
    // About 49 MiB retained sources, 50 MiB output, 50 MiB sourcesContent, 3.125 MiB
    // CR table, and about 6 MiB of root source, frontmatter, prefix and growth charges.
    let body = "b".repeat(50 * MIB - 5 * "x\n".len() - 2 + extra) + "\rc";
    let vars = HashMap::from([("body".to_owned(), Value::String(body))]);
    let opts = CompileOptions::default()
        .with_source_map(true)
        .with_include_sources_content(true);
    peak_growth(|| mds::compile_virtual_with_deps_opts(modules, "main.mds", Some(vars), opts))
}

fn assert_mapped_max_output_with_frontmatter() {
    let (control, peak) = mapped_corner(0);
    let control = control.unwrap();
    let sources = control
        .source_map
        .as_ref()
        .unwrap()
        .sources_content
        .as_ref()
        .unwrap();
    assert_eq!(sources.len(), 6);
    let source_bytes: usize = sources.iter().map(String::len).sum();
    assert!((49 * MIB..=50 * MIB).contains(&source_bytes));
    let output = control.into_markdown().unwrap();
    assert_eq!(output.len(), 51 * MIB + 8);
    assert!(output.starts_with("---\nlabel: a"));
    assert!(output.ends_with("bc\n"));
    assert!(!output.contains('\r'));
    drop(output);

    let (over, over_peak) = mapped_corner(1);
    eprintln!("mapped corner heap (B): MAX={peak}, MAX+1={over_peak}");
    assert_eq!(
        over.expect_err("output cap error").to_string(),
        "resource limit exceeded: output exceeds maximum size of 52428800 bytes"
    );
    // The control holds the 50 MiB output, so the check below reads real data.
    assert!(peak >= 50 * MIB, "mapped corner control: {peak} B");
    // One more output byte is refused, so its peak is at most 1 MiB above the control.
    assert!(
        over_peak <= peak + MIB,
        "the refused corner grows {over_peak} B (control: {peak} B)"
    );
}

fn imports_measured(part: &str, count: usize) -> (Result<CompileResult, MdsError>, usize) {
    let mut root = String::new();
    let mut modules = HashMap::new();
    for i in 0..count {
        root.push_str(&format!("@import \"./part{i}.mds\" as part{i}\n"));
        modules.insert(format!("part{i}.mds"), part.to_owned());
    }
    modules.insert("main.mds".to_owned(), root + "root");
    peak_growth(|| mds::compile_virtual(modules, "main.mds", None))
}

/// The densest syntax: each four bytes `\{{a` make two nodes. The node and token counts
/// sit just past a power of two, where a growing vector holds almost twice its length.
fn assert_syntax_tree_peaks() {
    let dense = "\\{{a".repeat((1 << 14) + 1);
    let function = format!("@define f():\n{dense}\n@end\n");
    let block = format!("@block b:\n{dense}\n@end\n");
    let levels = format!("{}{}", "@if a:\n".repeat(63), "@end\n".repeat(63));
    let nested = format!("---\na: ''\n---\n{}", levels.repeat(87));

    let (root_result, root_peak) = compile_measured(dense.clone());
    assert!(root_result
        .unwrap()
        .into_markdown()
        .unwrap()
        .starts_with("{{a{{a"));
    let (function_result, function_peak) = compile_measured(function.clone());
    // The copy of the function body is part of the measure.
    assert!(
        function_peak > root_peak,
        "function body {function_peak} B, root text {root_peak} B"
    );
    let cases = [
        (
            "uncalled function body",
            (function_result, function_peak),
            function.len(),
        ),
        ("imported text", imports_measured(&dense, 1), dense.len()),
        (
            "imported block default",
            imports_measured(&block, 1),
            block.len(),
        ),
        (
            "nested @if blocks",
            compile_measured(nested.clone()),
            nested.len(),
        ),
    ];
    for (label, (result, peak), len) in cases {
        let per_byte = peak.div_ceil(len);
        eprintln!("syntax tree heap (B): {label}: source={len}, peak={peak}, per byte={per_byte}");
        assert!(result.is_ok(), "{label}");
        assert!(
            peak <= TREE_PEAK_PER_BYTE * len,
            "{label}: {peak} B for {len} B of source"
        );
    }

    // The parser refuses the first directive, so the lexer finishes and no tree is built.
    let lexed = format!("@x\n{dense}");
    let (result, lexer_peak) = peak_growth(|| mds::scan_imports(&lexed));
    eprintln!("lexer heap (B): source={}, peak={lexer_peak}", lexed.len());
    let error = result.expect_err("unknown directive").serialize();
    assert_eq!(error.code, "mds::syntax");
    assert!(
        lexer_peak <= LEXER_PEAK_PER_BYTE * lexed.len(),
        "lexer: {lexer_peak} B"
    );

    // Each further import adds one cached module, which keeps its tree.
    let peaks = [1, 2, 3].map(|count| {
        let (result, peak) = imports_measured(&function, count);
        assert!(result.is_ok(), "{count} imports");
        peak
    });
    let kept = peaks[2] - peaks[1];
    eprintln!(
        "cached module heap (B): source={}, peaks={peaks:?}, kept={kept}",
        function.len()
    );
    assert!(
        kept <= TREE_KEPT_PER_BYTE * function.len(),
        "a cached module keeps {kept} B"
    );
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
    assert_unique_is_charged_before_it_copies();
    assert_message_role_peak();
    assert_mapped_max_output_with_frontmatter();
    assert_imported_prompts_share_one_limit();
    assert_alias_frontmatter_shares_one_limit();
    assert_extends_chain_trees_share_one_limit();
    assert_syntax_tree_peaks();
}
