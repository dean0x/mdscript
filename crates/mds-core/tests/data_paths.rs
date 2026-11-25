use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use mds::{FileSystem, MdsError, NativeFs, ReadLog, VirtualFs, MAX_FILE_SIZE};

/// Resolve `path` with the module resolver, then read the key with the data read. The
/// data read checks the key rules itself, so these tests do not depend on the checks of
/// the path as written.
fn read_data(fs: &dyn FileSystem, dir: &str, path: &str) -> Result<String, MdsError> {
    fs.read_data(&fs.normalize_in_dir(dir, path)?)
}

fn anchor(fs: &NativeFs, root: &Path) -> String {
    fs::write(root.join(".mdsroot"), "").unwrap();
    fs.anchor_base_dir(root.to_str().unwrap()).unwrap()
}

fn put(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn assert_error(error: &MdsError, code: &str, reason: &str) {
    assert_eq!(error.serialize().code, code, "{error}");
    assert!(
        error.to_string().contains(reason),
        "{error}; expected {reason}"
    );
}

#[cfg(unix)]
#[test]
fn native_data_refuses_final_links_and_directory_links_outside_the_root() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    put(&root, "modules/a.json", b"true");
    put(temp.path(), "other/a.json", b"false");
    let fs = NativeFs::new();
    let dir = anchor(&fs, &root);
    assert_eq!(read_data(&fs, &dir, "./modules/a.json").unwrap(), "true");

    fs::rename(root.join("modules/a.json"), root.join("real.json")).unwrap();
    symlink(root.join("real.json"), root.join("modules/a.json")).unwrap();
    let error = read_data(&fs, &dir, "./modules/a.json").unwrap_err();
    assert_error(&error, "mds::import", "symlinks are not allowed");

    fs::rename(root.join("modules"), root.join("saved")).unwrap();
    symlink(temp.path().join("other"), root.join("modules")).unwrap();
    let error = read_data(&fs, &dir, "./modules/a.json").unwrap_err();
    assert_error(&error, "mds::import", "escapes project directory");
}

#[cfg(unix)]
#[test]
fn native_data_checks_the_canonical_directory_but_not_root_ancestors() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join(".project");
    put(&root, "modules/a.json", b"true");
    put(&root, "NoDe_MoDuLeS/a.json", b"false");
    put(&root, ".hidden/a.json", b"false");
    let fs = NativeFs::new();
    let dir = anchor(&fs, &root);
    symlink(root.join("modules"), root.join("alias")).unwrap();
    assert_eq!(read_data(&fs, &dir, "./alias/a.json").unwrap(), "true");

    for (target, reason) in [
        ("NoDe_MoDuLeS", "node_modules"),
        (".hidden", "dot-prefixed"),
    ] {
        fs::remove_file(root.join("alias")).unwrap();
        symlink(root.join(target), root.join("alias")).unwrap();
        let error = read_data(&fs, &dir, "./alias/a.json").unwrap_err();
        assert_error(&error, "mds::import", reason);
    }
}

#[test]
fn native_data_caps_bytes_and_checks_utf8_before_parse() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let fs = NativeFs::new();
    let dir = anchor(&fs, root);
    let mut bytes = vec![b' '; MAX_FILE_SIZE as usize];
    bytes[..4].copy_from_slice(b"true");
    put(root, "exact.json", &bytes);
    bytes.push(b' ');
    put(root, "over.json", &bytes);
    put(root, "invalid.json", &[0xff]);
    assert_eq!(
        read_data(&fs, &dir, "./exact.json").unwrap().as_bytes(),
        &bytes[..bytes.len() - 1]
    );
    let error = read_data(&fs, &dir, "./over.json").unwrap_err();
    assert_error(&error, "mds::resource_limit", "file too large");
    let error = read_data(&fs, &dir, "./invalid.json").unwrap_err();
    assert_error(&error, "mds::io", "invalid UTF-8");
}

#[cfg(unix)]
#[test]
fn native_data_requires_the_resolved_file_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let fs = NativeFs::new();
    let dir = anchor(&fs, root);
    put(root, "data.json", b"true");
    let key = fs.normalize_in_dir(&dir, "./data.json").unwrap();
    assert_eq!(fs.read_data(&key).unwrap(), "true");

    fs::rename(root.join("data.json"), root.join("saved.json")).unwrap();
    put(root, "data.json", b"false");
    let error = fs.read_data(&key).unwrap_err();
    assert_error(&error, "mds::io", "file changed while it was opened");
    assert_eq!(read_data(&fs, &dir, "./data.json").unwrap(), "false");
}

#[test]
fn virtual_data_enforces_the_same_file_cap() {
    let at_cap = " ".repeat(MAX_FILE_SIZE as usize - 4) + "true";
    let over_cap = at_cap.clone() + " ";
    let fs = VirtualFs::new(HashMap::from([
        ("exact.json".into(), at_cap.clone()),
        ("over.json".into(), over_cap),
    ]));
    assert_eq!(read_data(&fs, "", "./exact.json").unwrap(), at_cap);
    let error = read_data(&fs, "", "./over.json").unwrap_err();
    assert_error(&error, "mds::resource_limit", "file too large");
}

#[test]
fn native_data_records_the_exact_byte_count() {
    let temp = tempfile::tempdir().unwrap();
    let log = Arc::new(ReadLog::new());
    let fs = NativeFs::new().with_read_log(Arc::clone(&log));
    let dir = anchor(&fs, temp.path());
    put(temp.path(), "data.json", b"true\n");
    assert_eq!(read_data(&fs, &dir, "./data.json").unwrap(), "true\n");
    let [(key, size)] = log.sizes().try_into().unwrap();
    assert!(Path::new(&key).ends_with("data.json"));
    assert_eq!(size, 5);
}

#[test]
fn native_and_virtual_data_misses_name_the_file() {
    let temp = tempfile::tempdir().unwrap();
    let fs = NativeFs::new();
    let dir = anchor(&fs, temp.path());
    put(temp.path(), "present.json", b"true");
    let virtual_fs = VirtualFs::new(HashMap::from([("present.json".into(), "true".into())]));
    for (fs, dir, code, reason) in [
        (
            &fs as &dyn FileSystem,
            dir.as_str(),
            "mds::file_not_found",
            "absent.json",
        ),
        (
            &virtual_fs,
            "",
            "mds::module_not_found",
            "data file 'absent.json'",
        ),
    ] {
        assert_eq!(read_data(fs, dir, "./present.json").unwrap(), "true");
        let error = read_data(fs, dir, "./absent.json").unwrap_err();
        assert_error(&error, code, reason);
    }
}

/// A compile names a missing data file as the declaring module writes it, also when an
/// import reached that module: the import adds its span and keeps the data file's name.
/// Controls: the entry's own miss has no span, and a missing module still names the
/// import as written.
#[test]
fn a_data_miss_in_an_imported_module_names_the_data_file() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let declares = b"---\ndata:\n  item: ./absent.json\n---\n{{item}}\n";
    put(root, ".mdsroot", b"");
    put(root, "entry.mds", declares);
    put(root, "sub/m.mds", declares);
    let import = "@import \"./sub/m.mds\" as m";
    put(root, "main.mds", format!("{import}\nHi\n").as_bytes());
    put(root, "gone.mds", b"@import \"./sub/gone.mds\" as g\nHi\n");
    for (entry, message, spanned) in [
        ("entry.mds", "file not found: ./absent.json", false),
        ("main.mds", "file not found: ./absent.json", true),
        ("gone.mds", "file not found: ./sub/gone.mds", true),
    ] {
        let error = mds::compile_with_deps(root.join(entry), None).unwrap_err();
        let serialized = error.serialize();
        assert_eq!(serialized.code, "mds::file_not_found", "{entry}: {error}");
        assert_eq!(serialized.message, message, "{entry}");
        let span = serialized.span.map(|span| (span.offset, span.line));
        assert_eq!(span.is_some(), spanned, "{entry}: {span:?}");
        if spanned {
            assert_eq!(span, Some((0, Some(1))), "{entry}");
        }
    }
}

#[test]
fn virtual_aliases_must_not_hide_a_blocked_key() {
    let fs = VirtualFs::new(HashMap::from([
        ("modules/a.json".into(), "true".into()),
        ("NoDe_MoDuLeS/a.json".into(), "false".into()),
    ]))
    .with_aliases(HashMap::from([
        ("good.json".into(), "modules/a.json".into()),
        ("bad.json".into(), "NoDe_MoDuLeS/a.json".into()),
    ]))
    .unwrap();
    assert_eq!(read_data(&fs, "", "./good.json").unwrap(), "true");
    let error = read_data(&fs, "", "./bad.json").unwrap_err();
    assert_error(&error, "mds::import", "node_modules");
}

/// `read_data` applies the segment rule to the key it gets. The module normalizer
/// does not apply that rule, so its key reaches the read-time check. The data path
/// rules refuse the same paths as written
/// (`data_path_rules_refuse_the_paths_that_the_read_check_gets` in
/// `crates/mds-core/src/data/path_tests.rs`). Control: a key of a permitted directory
/// from the same normalizer reads.
#[test]
fn read_data_checks_a_key_that_skipped_the_data_path_rule() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let fs = NativeFs::new();
    let dir = anchor(&fs, root);
    put(root, "modules/a.json", b"true");
    put(root, ".hidden/a.json", b"false");
    put(root, "NoDe_MoDuLeS/a.json", b"false");
    let control = fs.normalize_in_dir(&dir, "./modules/a.json").unwrap();
    assert_eq!(fs.read_data(&control).unwrap(), "true");
    for (path, reason) in [
        ("./.hidden/a.json", "dot-prefixed"),
        ("./NoDe_MoDuLeS/a.json", "node_modules"),
    ] {
        let key = fs.normalize_in_dir(&dir, path).unwrap();
        let error = fs.read_data(&key).unwrap_err();
        assert_error(&error, "mds::import", reason);
    }

    let virtual_fs = VirtualFs::new(HashMap::from([
        ("modules/a.json".into(), "true".into()),
        (".hidden/a.json".into(), "false".into()),
        ("NoDe_MoDuLeS/a.json".into(), "false".into()),
    ]));
    assert_eq!(virtual_fs.read_data("modules/a.json").unwrap(), "true");
    for (key, reason) in [
        (".hidden/a.json", "dot-prefixed"),
        ("NoDe_MoDuLeS/a.json", "node_modules"),
        ("/modules/a.json", "escapes project directory"),
    ] {
        let error = virtual_fs.read_data(key).unwrap_err();
        assert_error(&error, "mds::import", reason);
    }
}

const EXTENSION_REASON: &str = "expected the .json, .yaml or .yml extension";

/// A direct `read_data` call refuses a map key without a data extension, compared byte
/// for byte as a compile compares it, so `.JSON` is refused too. The module read serves
/// each refused key, so only the extension rule refuses it. Control: the same content
/// under a `.json` key reads.
#[test]
fn virtual_read_data_refuses_a_key_without_a_data_extension() {
    let fs = VirtualFs::new(HashMap::from([
        ("main.mds".into(), "true".into()),
        ("note.txt".into(), "true".into()),
        ("upper.JSON".into(), "true".into()),
        ("control.json".into(), "true".into()),
    ]));
    assert_eq!(fs.read_data("control.json").unwrap(), "true");
    for key in ["main.mds", "note.txt", "upper.JSON"] {
        assert_eq!(fs.read(key).unwrap(), "true", "{key}");
        let error = fs.read_data(key).unwrap_err();
        assert_error(&error, "mds::import", EXTENSION_REASON);
        assert!(
            error.to_string().contains(&format!("data path \"{key}\"")),
            "{error}"
        );
    }
}

/// A direct `read_data` call refuses a key that the module resolver registered below
/// the project root when the key has no data extension. The module read serves each
/// refused key, so only the extension rule refuses it, and the message names the file
/// relative to the project root. Control: the same content under a `.json` key reads.
#[test]
fn native_read_data_refuses_a_resolved_key_without_a_data_extension() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let fs = NativeFs::new();
    let dir = anchor(&fs, root);
    for name in ["note.txt", "upper.JSON", "control.json"] {
        put(root, name, b"true");
    }
    let control = fs.normalize_in_dir(&dir, "./control.json").unwrap();
    assert_eq!(fs.read_data(&control).unwrap(), "true");
    for name in ["note.txt", "upper.JSON"] {
        let key = fs.normalize_in_dir(&dir, &format!("./{name}")).unwrap();
        assert_eq!(fs.read(&key).unwrap(), "true", "{name}");
        let error = fs.read_data(&key).unwrap_err();
        assert_error(&error, "mds::import", EXTENSION_REASON);
        let message = error.to_string();
        assert!(
            message.contains(&format!("data path \"{name}\"")),
            "{message}"
        );
        assert!(!message.contains(&dir), "{message}");
    }
}

#[test]
fn backend_without_data_support_never_falls_back_to_module_read() {
    struct ModuleOnly;
    impl FileSystem for ModuleOnly {
        fn resolve_entry(&self, path: &str) -> Result<String, MdsError> {
            Ok(path.to_owned())
        }
        fn normalize_in_dir(&self, _dir: &str, path: &str) -> Result<String, MdsError> {
            Ok(path.trim_start_matches("./").to_owned())
        }
        fn parent_dir(&self, _key: &str) -> String {
            String::new()
        }
        fn read(&self, _key: &str) -> Result<String, MdsError> {
            Ok("module".to_owned())
        }
        fn is_markdown(&self, _key: &str) -> bool {
            false
        }
    }
    assert_eq!(ModuleOnly.read("data.json").unwrap(), "module");
    let error = read_data(&ModuleOnly, "", "./data.json").unwrap_err();
    assert_error(
        &error,
        "mds::io",
        "this file system does not read data files",
    );
}
