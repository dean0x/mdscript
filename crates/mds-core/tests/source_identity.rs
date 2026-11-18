#![cfg(any(unix, windows))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mds::{CompiledOutput, FileSystem, MdsError, ModuleCache, NativeFs};
use miette::Diagnostic;
use tempfile::TempDir;

fn lint_reads_gate_text(source: &str) -> bool {
    let body = source
        .split("pub fn lint_native_fs(")
        .nth(1)
        .unwrap_or("")
        .split("\n}")
        .next()
        .unwrap_or("");
    body.contains("let (_, source) = cache.resolve_path_intrinsic_source(")
        && body.contains("lint::lint_source(&source, filename, config)")
        && !body.contains("read_module_file")
        && !body.contains(".read(")
}

#[test]
fn lint_uses_the_gate_text_without_a_second_read() {
    let source = include_str!("../src/lib.rs");
    assert!(lint_reads_gate_text(source));
    let reread = source.replace(
        "let (_, source) = cache.resolve_path_intrinsic_source(",
        "let source = fs.read(",
    );
    assert!(!lint_reads_gate_text(&reread));
}

#[cfg(unix)]
#[test]
fn vars_refuses_a_character_device_with_the_changed_file_message() {
    let error = mds::load_vars_file(Path::new("/dev/null")).unwrap_err();
    assert_eq!(error.code().unwrap().to_string(), "mds::io");
    assert_eq!(
        error.to_string(),
        "cannot read null: the file changed while it was opened"
    );
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("vars.json");
    std::fs::write(&path, r#"{"name":"pipe"}"#).unwrap();
    assert_eq!(
        mds::load_vars_file(&path).unwrap()["name"],
        mds::Value::String("pipe".into())
    );
}

#[cfg(unix)]
#[test]
fn vars_from_a_named_pipe_still_compiles() {
    use std::io::Write;
    use std::sync::mpsc;
    use std::time::Duration;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("vars.json");
    assert!(std::process::Command::new("mkfifo")
        .arg(&path)
        .status()
        .unwrap()
        .success());
    let writer_path = path.clone();
    let writer = std::thread::spawn(move || {
        std::fs::File::create(writer_path)
            .unwrap()
            .write_all(br#"{"name":"pipe"}"#)
            .unwrap();
    });
    let (tx, rx) = mpsc::channel();
    let reader_path = path.clone();
    let reader = std::thread::spawn(move || {
        tx.send(mds::load_vars_file(&reader_path)).unwrap();
    });
    let result = rx.recv_timeout(Duration::from_secs(10));
    if !matches!(result, Ok(Ok(_))) {
        // Release a writer if the reader refused before it opened the pipe.
        let _release = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
    }
    writer.join().unwrap();
    let vars = result.expect("the pipe read completes").unwrap();
    reader.join().unwrap();
    assert_eq!(
        mds::compile_str_with("Hello {{name}}!", None, Some(vars))
            .unwrap()
            .output,
        CompiledOutput::Markdown("Hello pipe!\n".into())
    );
}

#[cfg(target_os = "macos")]
#[test]
fn vars_reads_a_pipe_through_dev_fd() {
    use std::os::fd::AsRawFd;
    use std::process::{Command, Stdio};

    let mut child = Command::new("printf")
        .arg(r#"{"name":"pipe"}"#)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let path = PathBuf::from(format!("/dev/fd/{}", stdout.as_raw_fd()));
    let vars = mds::load_vars_file(&path).unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(vars["name"], mds::Value::String("pipe".into()));
}

#[test]
fn native_entry_points_match_path_entry_points() {
    let dir = TempDir::new().unwrap();
    let entry = dir.path().join("main.mds");
    std::fs::write(dir.path().join("_part.mds"), "").unwrap();
    std::fs::write(
        &entry,
        "@import \"./_part.mds\" as part\n@include part\n{{name}}\n",
    )
    .unwrap();
    let vars = HashMap::from([("name".into(), mds::Value::String("Hello".into()))]);
    let opts = mds::CompileOptions::default().with_source_map(true);
    let pin = || {
        NativeFs::new().with_entry_pin(
            mds::EntryPin::key(entry.canonicalize().unwrap())
                .with_walked(&std::fs::symlink_metadata(&entry).unwrap()),
        )
    };
    let path = mds::compile_with_deps_opts(&entry, Some(vars.clone()), opts.clone()).unwrap();
    let native = mds::compile_native_fs(pin(), &entry, Some(vars.clone()), opts).unwrap();
    assert_eq!(native, path);
    assert_eq!(native.dependencies.len(), 1);
    assert!(!native.warnings.is_empty());
    let (_, warnings) = mds::check_collecting_warnings(&entry, Some(vars.clone())).unwrap();
    assert_eq!(
        mds::check_native_fs(pin(), &entry, Some(vars.clone())).unwrap(),
        warnings
    );
    let config = mds::LintConfig::default();
    assert_eq!(
        mds::lint_native_fs(pin(), &entry, Some(vars.clone()), &config)
            .unwrap()
            .to_canonical_json(),
        mds::lint(&entry, Some(vars), &config)
            .unwrap()
            .to_canonical_json()
    );
}

#[test]
fn native_entry_points_refuse_a_different_key_and_accept_the_control() {
    let dir = TempDir::new().unwrap();
    let entry = dir.path().join("main.mds");
    let other = dir.path().join("other.mds");
    std::fs::write(&entry, "Hello\n").unwrap();
    std::fs::write(&other, "Other\n").unwrap();
    for expected in [&other, &entry] {
        let fs =
            || NativeFs::new().with_entry_pin(mds::EntryPin::key(expected.canonicalize().unwrap()));
        let results = [
            mds::compile_native_fs(fs(), &entry, None, mds::CompileOptions::default()).map(|_| ()),
            mds::check_native_fs(fs(), &entry, None).map(|_| ()),
            mds::lint_native_fs(fs(), &entry, None, &mds::LintConfig::default()).map(|_| ()),
        ];
        for result in results {
            if expected == &other {
                let error = result.unwrap_err();
                assert_eq!(error.code().unwrap().to_string(), "mds::io");
                assert_eq!(
                    error.to_string(),
                    "cannot read main.mds: the file changed while it was opened"
                );
            } else {
                result.unwrap();
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn walked_entry_identity_refuses_a_replaced_file_and_accepts_the_control() {
    let dir = TempDir::new().unwrap();
    let entry = dir.path().join("main.mds");
    std::fs::write(&entry, "Hello\n").unwrap();
    let canonical = entry.canonicalize().unwrap();
    let walked = std::fs::symlink_metadata(&entry).unwrap();
    std::fs::rename(&entry, dir.path().join("saved.mds")).unwrap();
    std::fs::write(&entry, "Other\n").unwrap();
    for metadata in [walked, std::fs::symlink_metadata(&entry).unwrap()] {
        let pin = mds::EntryPin::key(&canonical).with_walked(&metadata);
        let result = NativeFs::new()
            .with_entry_pin(pin)
            .resolve_entry(entry.to_str().unwrap());
        use std::os::unix::fs::MetadataExt;
        if metadata.ino() != std::fs::symlink_metadata(&entry).unwrap().ino() {
            let error = result.unwrap_err();
            assert_eq!(error.code().unwrap().to_string(), "mds::io");
            assert_eq!(
                error.to_string(),
                "cannot read main.mds: the file changed while it was opened"
            );
        } else {
            assert_eq!(result.unwrap(), canonical.to_str().unwrap());
        }
    }
}

#[test]
fn parent_pin_accepts_a_new_name_in_the_same_directory_only() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("other")).unwrap();
    let entry = root.join("MAIN.mds");
    std::fs::write(&entry, "Hello\n").unwrap();
    for parent in [root.join("other"), root.clone()] {
        let fs = NativeFs::new().with_entry_pin(mds::EntryPin::parent_dir(&parent));
        let result = fs.resolve_entry(entry.to_str().unwrap());
        if parent == root {
            assert_eq!(fs.read(&result.unwrap()).unwrap(), "Hello\n");
        } else {
            assert_eq!(
                result.unwrap_err().to_string(),
                "cannot read MAIN.mds: the file changed while it was opened"
            );
        }
    }
}

#[derive(Clone, Copy)]
enum Change {
    Keep,
    ReplaceFile,
    ReplaceParent,
}

struct ChangeOnRead {
    inner: NativeFs,
    target: PathBuf,
    change: Change,
}

impl FileSystem for ChangeOnRead {
    fn resolve_entry(&self, path: &str) -> Result<String, MdsError> {
        self.inner.resolve_entry(path)
    }

    fn normalize_in_dir(&self, dir: &str, relative: &str) -> Result<String, MdsError> {
        self.inner.normalize_in_dir(dir, relative)
    }

    fn parent_dir(&self, key: &str) -> String {
        self.inner.parent_dir(key)
    }

    fn read(&self, key: &str) -> Result<String, MdsError> {
        if Path::new(key) == self.target {
            match self.change {
                Change::Keep => {}
                Change::ReplaceFile => {
                    std::fs::rename(&self.target, self.target.with_extension("saved")).unwrap();
                    std::fs::write(&self.target, "different\n").unwrap();
                }
                Change::ReplaceParent => {
                    let parent = self.target.parent().unwrap();
                    std::fs::rename(parent, parent.with_extension("saved")).unwrap();
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(parent.with_extension("other"), parent).unwrap();
                    #[cfg(windows)]
                    {
                        let output = std::process::Command::new("cmd")
                            .args(["/D", "/C", "mklink", "/J"])
                            .arg(mds::display_native_path(parent).as_ref())
                            .arg(mds::display_native_path(&parent.with_extension("other")).as_ref())
                            .output()
                            .expect("run mklink /J");
                        assert!(output.status.success(), "create a junction: {output:?}");
                        assert_eq!(
                            std::fs::read_to_string(&self.target).unwrap(),
                            "different\n",
                            "the replacement parent reaches the other file"
                        );
                    }
                }
            }
        }
        self.inner.read(key)
    }

    fn is_markdown(&self, key: &str) -> bool {
        self.inner.is_markdown(key)
    }

    fn anchor_base_dir(&self, dir: &str) -> Result<String, MdsError> {
        self.inner.anchor_base_dir(dir)
    }

    fn source_root(&self) -> Option<String> {
        self.inner.source_root()
    }
}

fn resolve_with_change(imported: bool, change: Change) -> Result<CompiledOutput, MdsError> {
    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::write(root.join(".mdsroot"), "").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::create_dir(root.join("sub.other")).unwrap();
    let target = root.join("sub").join("part.mds");
    std::fs::write(&target, "Hello\n").unwrap();
    std::fs::write(root.join("sub.other").join("part.mds"), "different\n").unwrap();
    let entry = if imported {
        let entry = root.join("main.mds");
        std::fs::write(&entry, "@import \"./sub/part.mds\"\nHello\n").unwrap();
        entry
    } else {
        target.clone()
    };
    let mut cache = ModuleCache::with_fs(Box::new(ChangeOnRead {
        inner: NativeFs::new(),
        target,
        change,
    }));
    cache.resolve_path_intrinsic(entry.to_str().unwrap(), &HashMap::new(), &mut Vec::new())
}

fn assert_changed(result: Result<CompiledOutput, MdsError>) {
    let error = result.expect_err("the file changed after resolve");
    assert_eq!(error.code().unwrap().to_string(), "mds::io");
    assert_eq!(
        error.to_string(),
        "cannot read sub/part.mds: the file changed while it was opened"
    );
}

#[cfg(unix)]
#[test]
fn entry_replaced_by_another_regular_file_is_refused() {
    assert_changed(resolve_with_change(false, Change::ReplaceFile));
}

#[test]
fn entry_with_a_changed_parent_is_refused() {
    assert_changed(resolve_with_change(false, Change::ReplaceParent));
}

#[cfg(unix)]
#[test]
fn import_replaced_by_another_regular_file_is_refused() {
    assert_changed(resolve_with_change(true, Change::ReplaceFile));
}

#[test]
fn import_with_a_changed_parent_is_refused() {
    assert_changed(resolve_with_change(true, Change::ReplaceParent));
}

#[cfg(windows)]
#[test]
fn entry_replaced_by_another_regular_file_of_the_same_name_reads() {
    assert_eq!(
        resolve_with_change(false, Change::ReplaceFile).unwrap(),
        CompiledOutput::Markdown("different\n".to_string())
    );
}

#[cfg(windows)]
#[test]
fn import_replaced_by_another_regular_file_of_the_same_name_reads() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let target = root.join("part.mds");
    std::fs::write(&target, "original\n").unwrap();
    let fs = ChangeOnRead {
        inner: NativeFs::new(),
        target: target.clone(),
        change: Change::ReplaceFile,
    };
    let key = fs
        .normalize_in_dir(root.to_str().unwrap(), "./part.mds")
        .unwrap();
    assert_eq!(fs.read(&key).unwrap(), "different\n");
}

#[cfg(windows)]
#[test]
fn unchanged_source_with_a_long_verbatim_path_reads() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let parent = root.join("a".repeat(100)).join("b".repeat(100));
    std::fs::create_dir_all(&parent).unwrap();
    let target = parent.join(format!("{}.mds", "c".repeat(100)));
    std::fs::write(&target, "Hello\n").unwrap();
    let fs = NativeFs::new();
    let key = fs.resolve_entry(target.to_str().unwrap()).unwrap();
    assert!(key.encode_utf16().count() >= 260);
    assert_eq!(fs.read(&key).unwrap(), "Hello\n");
}

#[test]
fn unchanged_entry_and_import_read_through_the_same_wrapper() {
    for imported in [false, true] {
        assert_eq!(
            resolve_with_change(imported, Change::Keep).unwrap(),
            CompiledOutput::Markdown("Hello\n".to_string())
        );
    }
}
