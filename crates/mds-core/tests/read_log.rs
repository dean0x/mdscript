//! `ReadLog` (#380): the byte count of each file a `NativeFs` compile or a logged `--vars`
//! load read, so a caller knows what the read saw without a later `stat`, which a save
//! that writes the file again can mislead.

use std::path::Path;
use std::sync::Arc;

use mds::{CompileOptions, NativeFs, ReadLog};

/// The count `log` holds for `path`, if it holds one. Keys are compared as the files they
/// name: on Windows a key drops the verbatim prefix that `canonicalize` gives.
fn count(log: &ReadLog, path: &Path) -> Option<u64> {
    let want = path.canonicalize().unwrap();
    log.sizes()
        .into_iter()
        .find(|(key, _)| Path::new(key).canonicalize().ok().as_ref() == Some(&want))
        .map(|(_, bytes)| bytes)
}

/// Every file a compile reads is recorded with the number of bytes the read gave: the entry
/// and an import, an empty one as 0.
#[test]
fn a_compile_records_the_byte_count_of_every_file_it_read() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("main.mds");
    let part = dir.path().join("part.mds");
    let empty = dir.path().join("empty.mds");
    std::fs::write(
        &entry,
        "@import \"./part.mds\" as p\n@import \"./empty.mds\" as e\n@include p\n@include e\n",
    )
    .unwrap();
    std::fs::write(&part, "Part.\n").unwrap();
    std::fs::write(&empty, "").unwrap();
    let log = Arc::new(ReadLog::new());

    let fs = NativeFs::new().with_read_log(Arc::clone(&log));
    let result = mds::compile_native_fs(fs, &entry, None, CompileOptions::default());

    assert!(result.is_ok(), "{result:?}");
    let entry_len = std::fs::metadata(&entry).unwrap().len();
    assert_eq!(count(&log, &entry), Some(entry_len));
    assert_eq!(count(&log, &part), Some(6));
    assert_eq!(count(&log, &empty), Some(0));
    assert_eq!(log.sizes().len(), 3, "{:?}", log.sizes());
}

#[test]
fn literal_directives_in_an_opted_in_fence_read_only_the_entry() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("main.mds");
    let part = dir.path().join("part.mds");
    std::fs::write(&part, "Part.\n").unwrap();
    let compile = |source: &str| {
        std::fs::write(&entry, source).unwrap();
        let log = Arc::new(ReadLog::new());
        let fs = NativeFs::new().with_read_log(Arc::clone(&log));
        let output = mds::compile_native_fs(fs, &entry, None, CompileOptions::default())
            .unwrap()
            .into_markdown()
            .unwrap();
        assert_eq!(count(&log, &entry), Some(source.len() as u64));
        (output, log)
    };

    let import_include = "@import \"./part.mds\" as p\n@include p\n";
    let extends = "@extends \"./part.mds\"\n";
    let literal = format!("{extends}{import_include}@for x in items:\n@define f():\n@message user:\n@block body:\n@export {{f}}\n");
    let (output, log) = compile(&format!("```+mds\n@if 1 == 1:\n{literal}@end\n```\n"));
    assert_eq!(output, format!("```\n{literal}```\n"));
    assert_eq!(log.sizes().len(), 1, "{:?}", log.sizes());

    for control in [import_include, extends] {
        let (output, log) = compile(control);
        assert_eq!(output, "Part.\n");
        assert_eq!(count(&log, &part), Some(6));
        assert_eq!(log.sizes().len(), 2, "{:?}", log.sizes());
    }
}

#[test]
fn the_file_key_never_reads_indented_imports_or_includes() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("main.mds");
    let part = dir.path().join("part.mds");
    std::fs::write(&part, "Part.\n").unwrap();

    for indent in ["  ", ""] {
        let directives = format!("{indent}@import \"./part.mds\" as p\n{indent}@include p\n");
        let source = format!(
            "---\nindented-directives: true\n---\n  @if 1 == 1:\nkeep\n  @end\n{directives}"
        );
        std::fs::write(&entry, &source).unwrap();
        let log = Arc::new(ReadLog::new());
        let fs = NativeFs::new().with_read_log(Arc::clone(&log));
        let output = mds::compile_native_fs(fs, &entry, None, CompileOptions::default())
            .unwrap()
            .into_markdown()
            .unwrap();
        assert_eq!(count(&log, &entry), Some(source.len() as u64));
        if indent.is_empty() {
            assert_eq!(output, "keep\nPart.\n");
            assert_eq!(count(&log, &part), Some(6));
            assert_eq!(log.sizes().len(), 2);
        } else {
            assert_eq!(output, format!("keep\n{directives}"));
            assert_eq!(log.sizes().len(), 1, "{:?}", log.sizes());
        }
    }
}

/// A compile that fails still records what it read before the failure: the empty import
/// that it failed on included.
#[test]
fn a_compile_that_fails_records_what_it_read() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("main.mds");
    let part = dir.path().join("part.mds");
    std::fs::write(&entry, "@import \"./part.mds\" as p\n{{p.missing}}\n").unwrap();
    std::fs::write(&part, "").unwrap();
    let log = Arc::new(ReadLog::new());

    let fs = NativeFs::new().with_read_log(Arc::clone(&log));
    let result = mds::compile_native_fs(fs, &entry, None, CompileOptions::default());

    assert!(result.is_err(), "the compile must fail for this test");
    assert_eq!(count(&log, &part), Some(0), "{:?}", log.sizes());
    assert!(count(&log, &entry).is_some_and(|bytes| bytes > 0));
}

/// The count is the one the read gave, not the file's size afterwards: a file written
/// again after the compile keeps the count of the read.
#[test]
fn the_count_is_the_read_s_not_the_file_s_later_size() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("main.mds");
    std::fs::write(&entry, "").unwrap();
    let log = Arc::new(ReadLog::new());

    let fs = NativeFs::new().with_read_log(Arc::clone(&log));
    let result = mds::compile_native_fs(fs, &entry, None, CompileOptions::default());
    std::fs::write(&entry, "Written after the read.\n").unwrap();

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(count(&log, &entry), Some(0));
}

/// A filesystem with no log records nothing, and a log no filesystem holds stays empty.
#[test]
fn no_log_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("main.mds");
    std::fs::write(&entry, "Hello\n").unwrap();
    let log = ReadLog::new();

    let result = mds::compile_native_fs(NativeFs::new(), &entry, None, CompileOptions::default());

    assert!(result.is_ok(), "{result:?}");
    assert!(log.sizes().is_empty());
}

/// A logged `--vars` load records the file's byte count when it read the bytes: an empty
/// file, which the load then refuses because it holds no JSON object, as 0.
#[test]
fn a_logged_vars_load_records_an_empty_file_it_refused() {
    let dir = tempfile::tempdir().unwrap();
    let vars = dir.path().join("vars.json");
    std::fs::write(&vars, "").unwrap();
    let log = ReadLog::new();

    let loaded = mds::load_vars_file_logged(&vars, &log);

    assert!(loaded.is_err(), "an empty vars file is refused");
    assert_eq!(count(&log, &vars), Some(0), "{:?}", log.sizes());
}

/// A logged `--vars` load that succeeds records the file's full byte count, and loads what
/// the unlogged load loads.
#[test]
fn a_logged_vars_load_records_the_file_it_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let vars = dir.path().join("vars.json");
    std::fs::write(&vars, "{\"a\": 1}").unwrap();
    let log = ReadLog::new();

    let loaded = mds::load_vars_file_logged(&vars, &log).unwrap();

    assert_eq!(
        loaded,
        mds::load_vars_file_reporting_duplicates(&vars).unwrap()
    );
    assert_eq!(count(&log, &vars), Some(8));
}
