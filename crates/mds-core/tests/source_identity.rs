#![cfg(any(unix, windows))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mds::{CompiledOutput, FileSystem, MdsError, ModuleCache, NativeFs};
use miette::Diagnostic;
use tempfile::TempDir;

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
