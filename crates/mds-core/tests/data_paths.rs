use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use mds::{FileSystem, MdsError, NativeFs, ReadLog, VirtualFs, MAX_FILE_SIZE};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    portable: Vec<Vector>,
    #[cfg(windows)]
    windows: Vec<Vector>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Vector {
    path: String,
    code: Option<String>,
    reason: Option<String>,
    code_point: Option<u32>,
    #[serde(default)]
    from: String,
}

fn read_data(fs: &dyn FileSystem, dir: &str, path: &str) -> Result<String, MdsError> {
    fs.read_data(&fs.normalize_data_in_dir(dir, path)?)
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

fn vectors() -> Vectors {
    serde_json::from_str(include_str!("fixtures/data-path-vectors.json")).unwrap()
}

#[test]
fn native_paths_follow_the_shared_portable_table() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    for path in [
        "modules/a.json",
        "modules/a.yaml",
        "modules/a.yml",
        "modules/a.JSON",
        "a.json",
        "modules/a.txt",
        "modules/{{name}}.json",
        ".env/a.json",
        "node_modules/a.json",
        "NoDe_MoDuLeS/a.json",
    ] {
        put(&root, path, b"true");
    }
    put(temp.path(), "a.json", b"false");
    let fs = NativeFs::new();
    let dir = anchor(&fs, &root);

    for vector in vectors().portable {
        let path = match vector.code_point {
            Some(point) => vector
                .path
                .replace("@CODEPOINT@", &char::from_u32(point).unwrap().to_string()),
            None => vector.path,
        };
        let from = if vector.from.is_empty() {
            dir.clone()
        } else {
            Path::new(&dir)
                .join(vector.from)
                .to_str()
                .unwrap()
                .to_owned()
        };
        let outcome = read_data(&fs, &from, &path);
        if let Some(code) = vector.code {
            let error = outcome.expect_err(&path);
            assert_error(&error, &code, &vector.reason.unwrap());
            assert!(error
                .to_string()
                .chars()
                .all(|ch| !mds::is_forbidden_path_char(ch)));
        } else {
            assert_eq!(outcome.unwrap(), "true", "{path}");
        }
    }
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

#[cfg(windows)]
#[test]
fn windows_data_refuses_trailing_dot_and_short_name_forms() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    put(root, "modules/a.json", b"true");
    put(root, "node_modules/a.json", b"false");
    let fs = NativeFs::new();
    let dir = anchor(&fs, root);
    assert_eq!(read_data(&fs, &dir, "./modules/a.json").unwrap(), "true");

    // If this volume has no 8.3 alias, give the same spelling a directory junction.
    // Either form must resolve to the blocked parent before the segment check.
    let short = root.join("NODE_M~1");
    if !short.exists() {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&short)
            .arg(root.join("node_modules"))
            .status()
            .unwrap();
        assert!(status.success());
    }
    let short_key = fs.normalize_in_dir(&dir, "./NODE_M~1/a.json").unwrap();
    let long_key = fs.normalize_in_dir(&dir, "./node_modules/a.json").unwrap();
    assert_eq!(short_key, long_key);
    for vector in vectors().windows {
        let error = read_data(&fs, &dir, &vector.path).unwrap_err();
        assert_error(&error, &vector.code.unwrap(), &vector.reason.unwrap());
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
    let key = fs.normalize_data_in_dir(&dir, "./data.json").unwrap();
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
