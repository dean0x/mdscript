use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::normalize;
use crate::{FileSystem, MdsError, NativeFs};

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

fn vectors() -> Vectors {
    serde_json::from_str(include_str!("../../tests/fixtures/data-path-vectors.json")).unwrap()
}

/// Apply the data path rules of a compile to `path` as written, then read the key.
fn read_data(fs: &NativeFs, dir: &str, path: &str) -> Result<String, MdsError> {
    fs.read_data(&normalize(fs, dir, path)?)
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
                .all(|ch| !crate::is_forbidden_path_char(ch)));
        } else {
            assert_eq!(outcome.unwrap(), "true", "{path}");
        }
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

/// The data path rules refuse the dot-prefixed and `node_modules` paths that
/// `read_data_checks_a_key_that_skipped_the_data_path_rule` in
/// `crates/mds-core/tests/data_paths.rs` resolves with the module resolver. Those files
/// do not exist here, so the refusal comes from the check of the path as written,
/// before resolution: an absent file is `mds::file_not_found` after resolution.
/// Control: a path to a permitted directory resolves.
#[test]
fn data_path_rules_refuse_the_paths_that_the_read_check_gets() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let fs = NativeFs::new();
    let dir = anchor(&fs, root);
    put(root, "modules/a.json", b"true");
    assert_eq!(read_data(&fs, &dir, "./modules/a.json").unwrap(), "true");
    for (path, reason) in [
        ("./.hidden/a.json", "dot-prefixed"),
        ("./NoDe_MoDuLeS/a.json", "node_modules"),
    ] {
        let error = normalize(&fs, &dir, path).unwrap_err();
        assert_error(&error, "mds::import", reason);
        assert!(
            error.to_string().contains(&format!("data path \"{path}\"")),
            "{error}"
        );
    }
}
