//! Each local data limit through the public compile API: a compile at the cap passes,
//! and one more unit is refused with the message of that cap. The messages name the
//! values of the crate-private constants in `limits.rs`, so a changed value fails here.

use std::collections::HashMap;

use mds::{compile_virtual_fs, CompileOptions, CompileResult, MdsError, VirtualFs};

const MIB: usize = 1024 * 1024;
const FILE_BYTES: usize = 10 * MIB;
const NODES_PER_FILE: usize = 200_000;
const ENTRIES_PER_MODULE: usize = 64;
const ENTRIES_PER_COMPILE: usize = 256;
const REFS_PER_ENTRY: usize = 256;

fn compile(files: HashMap<String, String>) -> Result<CompileResult, MdsError> {
    compile_virtual_fs(
        VirtualFs::new(files),
        "main.mds",
        None,
        CompileOptions::default(),
    )
}

fn rendered(result: Result<CompileResult, MdsError>) -> String {
    result.unwrap().into_markdown().unwrap()
}

fn limit_message(result: Result<CompileResult, MdsError>) -> String {
    let error = result.unwrap_err().serialize();
    assert_eq!(error.code, "mds::resource_limit", "{}", error.message);
    error.message
}

/// A frontmatter that declares `count` entries `<prefix>0..`, each naming `file(i)`.
fn declarations(prefix: &str, count: usize, file: impl Fn(usize) -> String) -> String {
    let mut source = String::from("---\ndata:\n");
    for i in 0..count {
        source.push_str(&format!("  {prefix}{i}: {}\n", file(i)));
    }
    source + "---\n"
}

/// An entry module and `parts.len()` imported modules. Part `i` declares `parts[i]`
/// entries; each entry names `file(part, entry)`.
fn modules(parts: &[usize], file: impl Fn(usize, usize) -> String) -> HashMap<String, String> {
    let mut main = String::new();
    let mut files = HashMap::new();
    for (part, &count) in parts.iter().enumerate() {
        main.push_str(&format!("@import \"./p{part}.mds\" as p{part}\n"));
        let source = declarations("e", count, |entry| file(part, entry)) + "Part.\n";
        files.insert(format!("p{part}.mds"), source);
    }
    files.insert("main.mds".into(), main + "ok\n");
    files
}

#[test]
fn file_byte_cap_passes_a_low_node_file_of_long_strings_and_refuses_one_more_byte() {
    // Five nodes, so this file reaches the byte cap long before the node cap.
    let file = |extra: usize| {
        let head = r#"{"short":"ok","long":""#;
        let fill = FILE_BYTES - head.len() - r#""}"#.len() + extra;
        format!("{head}{}\"}}", "x".repeat(fill))
    };
    let source = "---\ndata:\n  item: ./item.json\n---\n{{item.short}} {{length(item.long)}}\n";
    let files = |extra| {
        let text = file(extra);
        assert_eq!(text.len(), FILE_BYTES + extra);
        HashMap::from([
            ("main.mds".to_owned(), source.to_owned()),
            ("item.json".to_owned(), text),
        ])
    };
    let fill = FILE_BYTES - r#"{"short":"ok","long":""}"#.len();
    assert!(rendered(compile(files(0))).ends_with(&format!("ok {fill}\n")));
    assert_eq!(
        limit_message(compile(files(1))),
        format!(
            "resource limit exceeded: file too large ({} bytes, max {FILE_BYTES} bytes): item.json",
            FILE_BYTES + 1
        )
    );
}

#[test]
fn node_cap_passes_a_dense_file_at_the_cap_and_refuses_one_more_node() {
    // The root array and its elements are the nodes of the file.
    let files = |nodes: usize| {
        let text = format!("[{}0]", "0,".repeat(nodes - 2));
        let source = "---\ndata:\n  item: ./item.json\n---\n{{length(item)}}\n";
        HashMap::from([
            ("main.mds".to_owned(), source.to_owned()),
            ("item.json".to_owned(), text),
        ])
    };
    let elements = NODES_PER_FILE - 1;
    assert!(rendered(compile(files(NODES_PER_FILE))).ends_with(&format!("{elements}\n")));
    assert_eq!(
        limit_message(compile(files(NODES_PER_FILE + 1))),
        format!(
            "resource limit exceeded: data file JSON node count exceeds maximum of {NODES_PER_FILE}"
        )
    );
}

#[test]
fn module_entry_cap_passes_the_cap_and_refuses_one_more_entry() {
    let files = |count| {
        let source = declarations("e", count, |_| "./v.json".into());
        let last = count - 1;
        HashMap::from([
            (
                "main.mds".to_owned(),
                format!("{source}{{{{e0}}}}{{{{e{last}}}}}\n"),
            ),
            ("v.json".to_owned(), "1".to_owned()),
        ])
    };
    assert!(rendered(compile(files(ENTRIES_PER_MODULE))).ends_with("11\n"));
    assert_eq!(
        limit_message(compile(files(ENTRIES_PER_MODULE + 1))),
        format!(
            "resource limit exceeded: \"main.mds\": too many data entries (max {ENTRIES_PER_MODULE} per module)"
        )
    );
}

#[test]
fn compile_entry_cap_counts_entries_across_modules_not_files() {
    // Every entry names one file, so the memo reads it once.
    let mut files = modules(&[64, 64, 64, 64], |_, _| "./v.json".into());
    files.insert("v.json".into(), "1".into());
    assert_eq!(rendered(compile(files.clone())), "ok\n");
    let mut over = modules(&[64, 64, 64, 64, 1], |_, _| "./v.json".into());
    over.insert("v.json".into(), "1".into());
    assert_eq!(
        limit_message(compile(over)),
        format!(
            "resource limit exceeded: too many data entries (max {ENTRIES_PER_COMPILE} per compile)"
        )
    );
}

#[test]
fn distinct_file_cap_passes_and_the_entry_cap_binds_before_the_file_cap() {
    // Each read needs an entry, and the entry cap counts entries before reads. So the
    // 257th distinct file is refused by the entry cap; the resolver unit test
    // `data_file_cap_refuses_a_new_file_past_the_cap` proves the file cap itself.
    let distinct = |part: usize, entry: usize| format!("./d{part}_{entry}.json");
    let with_files = |parts: &[usize]| {
        let mut files = modules(parts, distinct);
        for (part, &count) in parts.iter().enumerate() {
            for entry in 0..count {
                files.insert(distinct(part, entry)[2..].to_owned(), "1".into());
            }
        }
        files
    };
    let at_cap = with_files(&[64, 64, 64, 64]);
    assert_eq!(at_cap.len(), 5 + 256);
    assert_eq!(rendered(compile(at_cap)), "ok\n");
    assert_eq!(
        limit_message(compile(with_files(&[64, 64, 64, 64, 1]))),
        format!(
            "resource limit exceeded: too many data entries (max {ENTRIES_PER_COMPILE} per compile)"
        )
    );
}

#[test]
fn level_cap_passes_the_cap_before_url_denial_and_refuses_one_more_level() {
    let chain = |levels: usize| {
        let mut source = String::from("---\ndata:\n  entry0: https://example.com/root\n");
        for level in 1..levels {
            source.push_str(&format!(
                "  entry{level}: 'https://example.com/{{{{entry{}}}}}'\n",
                level - 1
            ));
        }
        HashMap::from([("main.mds".to_owned(), source + "---\nHello.\n")])
    };
    // At the cap the graph passes, and the unreplayed URL entries reach the denial.
    let error = compile(chain(16)).unwrap_err().serialize();
    assert_eq!(error.code, "mds::network_denied", "{}", error.message);
    assert_eq!(
        limit_message(compile(chain(17))),
        "resource limit exceeded: data dependency graph exceeds 16 levels"
    );
}

#[test]
fn reference_cap_counts_the_fields_of_one_entry_and_binds_before_the_graph_check() {
    // An entry puts its references in the URL, a header and the body, and the cap
    // counts them together. Each entry has its own count. The reference past the cap
    // names nothing in scope, so only a refusal before the graph check gives the limit.
    let entry = |name: &str, extra: &str| {
        let body = "{{k}}".repeat(REFS_PER_ENTRY - 2) + extra;
        format!(
            "  {name}:\n    url: 'https://example.com/{{{{k}}}}'\n    method: POST\n    headers:\n      X-Key: '{{{{k}}}}'\n    body:\n      text: '{body}'\n"
        )
    };
    let files = |extra: &str| {
        let data = entry("a", "") + &entry("b", extra);
        HashMap::from([(
            "main.mds".to_owned(),
            format!("---\nk: v\ndata:\n{data}---\nHello.\n"),
        )])
    };
    // At the cap the graph passes, and the unreplayed URL entries reach the denial.
    let error = compile(files("")).unwrap_err().serialize();
    assert_eq!(error.code, "mds::network_denied", "{}", error.message);
    assert_eq!(
        limit_message(compile(files("{{missing}}"))),
        format!(
            "resource limit exceeded: \"main.mds\", entry \"b\": too many references (max {REFS_PER_ENTRY} per entry)"
        )
    );
}

#[test]
fn compile_byte_cap_passes_distinct_files_at_the_cap_and_refuses_one_more_byte() {
    // Four files of one value node each: the aggregate binds before any per-file cap.
    let source = "---\ndata:\n  a: ./a.json\n  b: ./b.json\n  c: ./c.json\n  d: ./d.json\n---\n{{a}}{{b}}{{c}}{{d}}\n";
    let mut text = " ".repeat(8 * MIB - 1);
    text.push('7');
    let files = |last: String| {
        HashMap::from([
            ("main.mds".to_owned(), source.to_owned()),
            ("a.json".to_owned(), text.clone()),
            ("b.json".to_owned(), text.clone()),
            ("c.json".to_owned(), text.clone()),
            ("d.json".to_owned(), last),
        ])
    };
    assert!(rendered(compile(files(text.clone()))).ends_with("7777\n"));
    assert_eq!(
        limit_message(compile(files(format!("{text} ")))),
        format!(
            "resource limit exceeded: data files exceed {} bytes per compile",
            32 * MIB
        )
    );
}
