//! #265: a path carrying a forbidden path character is refused at the input boundary.
//!
//! The class is [`mds::is_forbidden_path_char`] — 80 codepoints (C0 incl. LF and TAB,
//! DEL, C1, the bidi/format hazards). Import strings are refused with `mds::import`;
//! entry paths, entry keys and base directories with `mds::io`. Every message names the
//! offending codepoint as `U+XXXX`, shows the path the caller typed escaped by
//! [`mds::escape_path_for_message`], and so carries no forbidden character itself.
//!
//! Black-box: every test drives the public API. PF-018: each hostile character is
//! built at runtime (`char::from_u32`, `'\x1b'`) and every six-character escape text
//! with `format!`, so no live control byte is ever written into this file.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use mds::{is_forbidden_path_char, FileSystem, MdsError, ModuleCache, VirtualFs};

// ── Helpers ─────────────────────────────────────────────────────────────────

const ESC: char = '\x1b';
/// U+202E RIGHT-TO-LEFT OVERRIDE.
const RLO: char = '\u{202E}';

/// Every forbidden codepoint, with a non-vacuity pin on the class size.
fn forbidden_chars() -> Vec<char> {
    let all: Vec<char> = (0..=0x10_FFFF_u32)
        .filter_map(char::from_u32)
        .filter(|&c| is_forbidden_path_char(c))
        .collect();
    assert_eq!(
        all.len(),
        80,
        "non-vacuity: the forbidden class is 80 codepoints"
    );
    all
}

/// The `mds::…` diagnostic code of an error.
fn code_of(err: &MdsError) -> String {
    miette::Diagnostic::code(err)
        .map(|c| c.to_string())
        .unwrap_or_default()
}

/// `U+XXXX`, uppercase, as every rejection names the codepoint.
fn u_plus(ch: char) -> String {
    format!("U+{:04X}", u32::from(ch))
}

/// The six-character escape text `escape_path_for_message` writes for `ch`.
fn escaped(ch: char) -> String {
    format!("\\u{:04X}", u32::from(ch))
}

/// Assert `msg` carries no forbidden character (TAB and LF included).
fn assert_no_forbidden(msg: &str, label: &str) {
    if let Some(ch) = msg.chars().find(|&c| is_forbidden_path_char(c)) {
        panic!("{label}: message carries raw {} — {msg:?}", u_plus(ch));
    }
}

fn vfs(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// A virtual `main.mds` whose frontmatter imports `path_yaml` (YAML double-quoted
/// scalar text, so YAML escapes are decoded by the parser).
fn frontmatter_import(path_yaml: &str) -> HashMap<String, String> {
    vfs(&[(
        "main.mds",
        &format!("---\nimports:\n  - path: \"{path_yaml}\"\n---\nHi\n"),
    )])
}

// ── Positive control: legitimate non-ASCII paths are untouched ──────────────

/// Spaces, `-` and non-ASCII letters are not in the class: a module named with all
/// of them resolves as an import on the virtual and the native filesystem alike.
#[test]
fn unicode_and_space_path_is_accepted() {
    let name = "a b-\u{00FC}n\u{00EF}.mds";
    for ch in name.chars() {
        assert!(
            !is_forbidden_path_char(ch),
            "control: {ch:?} must be allowed"
        );
    }
    let lib = "@define greet(x):\nHello {{x}}!\n@end\n";
    let main = format!("@import \"./{name}\"\n{{{{greet(\"there\")}}}}\n");

    let out = mds::compile_virtual(vfs(&[(name, lib), ("main.mds", &main)]), "main.mds", None)
        .expect("virtual import of a Unicode name must compile")
        .into_markdown()
        .unwrap();
    assert_eq!(out, "Hello there!\n");

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".mdsroot"), "").unwrap();
    std::fs::write(dir.path().join(name), lib).unwrap();
    std::fs::write(dir.path().join("main.mds"), &main).unwrap();
    let out = mds::compile(dir.path().join("main.mds"), None)
        .expect("native import of a Unicode name must compile")
        .into_markdown()
        .unwrap();
    assert_eq!(out, "Hello there!\n");
}

// ── Import strings → mds::import ────────────────────────────────────────────

/// All 80 codepoints in a frontmatter `imports:` path, written as the YAML escape
/// for each (the only syntax that can carry LF and CR into a path). The error names
/// the REAL reason — the forbidden codepoint, or the null byte — not a generic
/// "must start with './'".
#[test]
fn frontmatter_import_refuses_every_forbidden_char() {
    let chars = forbidden_chars();
    for &ch in &chars {
        let yaml_path = format!("./a{}.mds", escaped(ch));
        let err = mds::compile_virtual(frontmatter_import(&yaml_path), "main.mds", None)
            .expect_err("a forbidden char in a frontmatter import must be refused");
        let label = u_plus(ch);
        assert_eq!(code_of(&err), "mds::import", "{label}: {err:?}");
        let msg = err.to_string();
        let reason = if ch == '\0' {
            "contains null byte".to_string()
        } else {
            format!("contains forbidden character {label}")
        };
        let expected = format!(
            "imports[0]: invalid path \"./a{}.mds\": {reason} (in frontmatter)",
            escaped(ch)
        );
        assert!(
            msg.contains(&expected),
            "{label}: expected {expected:?} in {msg:?}"
        );
        assert_no_forbidden(&msg, &label);
    }
}

/// The two YAML escapes the plan names — `\e` (ESC) and the escape for U+202E —
/// written as a YAML author would type them.
#[test]
fn frontmatter_import_yaml_escapes_name_the_forbidden_char() {
    for (yaml_path, ch) in [
        ("./evil\\e.mds".to_string(), ESC),
        (format!("./evil{}.mds", escaped(RLO)), RLO),
    ] {
        let err = mds::compile_virtual(frontmatter_import(&yaml_path), "main.mds", None)
            .expect_err("must be refused");
        assert_eq!(code_of(&err), "mds::import", "{err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("contains forbidden character {}", u_plus(ch))),
            "{yaml_path}: {msg}"
        );
        assert!(msg.contains(&format!("./evil{}.mds", escaped(ch))), "{msg}");
        assert_no_forbidden(&msg, &yaml_path);
    }
}

/// Control: a frontmatter path that breaks only the relative-form rule still reports
/// THAT reason, and a clean relative one gets past validation to resolution.
#[test]
fn frontmatter_import_reports_not_relative_as_its_reason() {
    let err = mds::compile_virtual(frontmatter_import("lib.mds"), "main.mds", None)
        .expect_err("a bare name is not a relative import");
    assert_eq!(code_of(&err), "mds::import");
    assert!(
        err.to_string()
            .contains("imports[0]: invalid path \"lib.mds\": must start with './' or '../'"),
        "{err}"
    );

    let err = mds::compile_virtual(frontmatter_import("./lib.mds"), "main.mds", None)
        .expect_err("control: the module does not exist");
    assert!(
        !err.to_string().contains("invalid path"),
        "control: a clean path passes validation and fails at resolution: {err}"
    );
}

/// A body `@import` carrying a raw forbidden character. LF and CR end the directive
/// line, so the string cannot hold them (the frontmatter test covers both); the other
/// 78 all reach the import-path check.
#[test]
fn body_import_refuses_forbidden_chars() {
    let chars: Vec<char> = forbidden_chars()
        .into_iter()
        .filter(|&c| c != '\n' && c != '\r')
        .collect();
    assert_eq!(chars.len(), 78, "non-vacuity");
    for ch in chars {
        let label = u_plus(ch);
        let main = format!("@import \"./lib{ch}.mds\"\nHi\n");
        let lib_key = format!("lib{ch}.mds");
        let modules = vfs(&[("main.mds", &main), (&lib_key, "@define f():\nx\n@end\n")]);
        let err = mds::compile_virtual(modules, "main.mds", None)
            .expect_err("a forbidden char in an @import must be refused");
        assert_eq!(code_of(&err), "mds::import", "{label}: {err:?}");
        let msg = err.to_string();
        let expected = if ch == '\0' {
            "import path contains null byte".to_string()
        } else {
            format!(
                "import path contains forbidden character {label}: \"./lib{}.mds\"",
                escaped(ch)
            )
        };
        assert!(
            msg.contains(&expected),
            "{label}: expected {expected:?} in {msg:?}"
        );
        assert_no_forbidden(&msg, &label);
    }
}

#[test]
fn extends_refuses_forbidden_chars() {
    for ch in [ESC, RLO, '\t'] {
        let main = format!("@extends \"./base{ch}.mds\"\n@block body:\nchild\n@end\n");
        let base_key = format!("base{ch}.mds");
        let modules = vfs(&[
            ("main.mds", &main),
            (&base_key, "@block body:\nbase\n@end\n"),
        ]);
        let err = mds::compile_virtual(modules, "main.mds", None)
            .expect_err("a forbidden char in @extends must be refused");
        assert_eq!(code_of(&err), "mds::import", "{err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains(&format!(
                "import path contains forbidden character {}: \"./base{}.mds\"",
                u_plus(ch),
                escaped(ch)
            )),
            "{msg}"
        );
        assert_no_forbidden(&msg, "@extends");
    }
}

// ── Entry paths and keys → mds::io ──────────────────────────────────────────

/// Every virtual entry API refuses a key carrying any of the 80 (this is the WASM
/// entry-key path: the binding passes its `filename` straight through).
#[test]
fn virtual_entry_key_refuses_every_forbidden_char() {
    for ch in forbidden_chars() {
        let label = u_plus(ch);
        let key = format!("main{ch}.mds");
        let modules = || vfs(&[(&key, "Hi\n")]);
        let expected = if ch == '\0' {
            format!("entry path contains null byte: \"main{}.mds\"", escaped(ch))
        } else {
            format!(
                "entry path contains forbidden character {label}: \"main{}.mds\"",
                escaped(ch)
            )
        };
        let errors = [
            mds::compile_virtual(modules(), &key, None).map(|_| ()),
            mds::check_virtual(modules(), &key, None),
            mds::lint_virtual(modules(), &key, None, &mds::LintConfig::default()).map(|_| ()),
        ];
        for (api, result) in ["compile_virtual", "check_virtual", "lint_virtual"]
            .into_iter()
            .zip(errors)
        {
            let err = result.expect_err("a forbidden char in an entry key must be refused");
            assert!(matches!(err, MdsError::Io { .. }), "{api} {label}: {err:?}");
            assert_eq!(code_of(&err), "mds::io", "{api} {label}");
            let msg = err.to_string();
            assert!(
                msg.contains(&expected),
                "{api} {label}: {expected:?} in {msg:?}"
            );
            assert_no_forbidden(&msg, &label);
        }
    }
}

#[test]
fn module_cache_virtual_entry_refuses_forbidden_char() {
    let key = format!("main{ESC}.mds");
    let mut cache = ModuleCache::virtual_fs(vfs(&[(&key, "Hi\n")]));
    let err = cache
        .resolve_virtual_intrinsic(&key, &HashMap::new(), &mut vec![])
        .expect_err("resolve_virtual_intrinsic must refuse the key");
    assert_eq!(code_of(&err), "mds::io");

    let mut cache = ModuleCache::virtual_fs(vfs(&[(&key, "Hi\n")]));
    let err = cache
        .resolve_key(&key, &HashMap::new(), &mut vec![])
        .expect_err("resolve_key must refuse the key");
    assert_eq!(code_of(&err), "mds::io");
    assert!(err.to_string().contains("U+001B"), "{err}");
}

/// A typed native entry path is refused before the filesystem is touched, so this
/// needs nothing on disk and runs on every platform.
#[test]
fn native_entry_path_refuses_forbidden_chars() {
    for ch in [ESC, '\n', '\t', RLO, '\u{0085}'] {
        let typed = format!("./main{ch}.mds");
        let label = u_plus(ch);
        let expected = format!(
            "entry path contains forbidden character {label}: \"./main{}.mds\"",
            escaped(ch)
        );
        let errors = [
            mds::compile(Path::new(&typed), None).map(|_| ()),
            mds::check(Path::new(&typed), None),
            mds::lint(Path::new(&typed), None, &mds::LintConfig::default()).map(|_| ()),
        ];
        for result in errors {
            let err = result.expect_err("must be refused");
            assert_eq!(code_of(&err), "mds::io", "{label}: {err:?}");
            let msg = err.to_string();
            assert!(msg.contains(&expected), "{label}: {expected:?} in {msg:?}");
            assert_no_forbidden(&msg, &label);
        }
    }
}

/// The base directory of a string compile is refused on its typed form.
#[test]
fn string_api_base_dir_refuses_forbidden_chars() {
    for ch in [ESC, '\t', RLO] {
        let typed = format!("templates{ch}dir");
        let label = u_plus(ch);
        let expected = format!(
            "base directory contains forbidden character {label}: \"templates{}dir\"",
            escaped(ch)
        );
        let base = Some(Path::new(&typed));
        let errors = [
            mds::compile_str_with("Hi\n", base, None).map(|_| ()),
            mds::check_str_with("Hi\n", base, None),
            mds::lint_str_with("Hi\n", base, None, &mds::LintConfig::default()).map(|_| ()),
        ];
        for result in errors {
            let err = result.expect_err("must be refused");
            assert_eq!(code_of(&err), "mds::io", "{label}: {err:?}");
            let msg = err.to_string();
            assert!(msg.contains(&expected), "{label}: {expected:?} in {msg:?}");
            assert_no_forbidden(&msg, &label);
        }
    }
}

// ── Custom backends: the resolver refuses before the backend is called ──────

/// A `VirtualFs` wrapper that counts every call a forbidden path could reach.
struct CountingFs {
    inner: VirtualFs,
    entries: Arc<AtomicUsize>,
    imports: Arc<AtomicUsize>,
    anchors: Arc<AtomicUsize>,
}

impl FileSystem for CountingFs {
    fn resolve_entry(&self, path: &str) -> Result<String, MdsError> {
        self.entries.fetch_add(1, Ordering::SeqCst);
        // Identity, with none of the built-in checks: a backend that forgot them.
        Ok(path.to_string())
    }
    fn normalize_in_dir(&self, dir: &str, relative: &str) -> Result<String, MdsError> {
        self.imports.fetch_add(1, Ordering::SeqCst);
        self.inner.normalize_in_dir(dir, relative)
    }
    fn parent_dir(&self, key: &str) -> String {
        self.inner.parent_dir(key)
    }
    fn read(&self, normalized: &str) -> Result<String, MdsError> {
        self.inner.read(normalized)
    }
    fn is_markdown(&self, normalized: &str) -> bool {
        self.inner.is_markdown(normalized)
    }
    fn anchor_base_dir(&self, dir: &str) -> Result<String, MdsError> {
        self.anchors.fetch_add(1, Ordering::SeqCst);
        Ok(dir.to_string())
    }
}

struct Counters {
    entries: Arc<AtomicUsize>,
    imports: Arc<AtomicUsize>,
    anchors: Arc<AtomicUsize>,
}

fn counting_cache(modules: HashMap<String, String>) -> (ModuleCache, Counters) {
    let counters = Counters {
        entries: Arc::default(),
        imports: Arc::default(),
        anchors: Arc::default(),
    };
    let fs = CountingFs {
        inner: VirtualFs::new(modules),
        entries: Arc::clone(&counters.entries),
        imports: Arc::clone(&counters.imports),
        anchors: Arc::clone(&counters.anchors),
    };
    (ModuleCache::with_fs(Box::new(fs)), counters)
}

#[test]
fn custom_backend_never_sees_a_forbidden_entry_key() {
    let key = format!("main{ESC}.mds");
    let (mut cache, n) = counting_cache(vfs(&[(&key, "Hi\n")]));
    let err = cache
        .resolve_key(&key, &HashMap::new(), &mut vec![])
        .expect_err("the resolver must refuse the key");
    assert_eq!(code_of(&err), "mds::io");
    assert_eq!(
        n.entries.load(Ordering::SeqCst),
        0,
        "backend must not be called"
    );

    // Positive control: a clean key does reach the backend's resolve_entry.
    let (mut cache, n) = counting_cache(vfs(&[("main.mds", "Hi\n")]));
    cache
        .resolve_key("main.mds", &HashMap::new(), &mut vec![])
        .expect("control compiles");
    assert_eq!(
        n.entries.load(Ordering::SeqCst),
        1,
        "control: counter counts"
    );
}

#[test]
fn custom_backend_never_sees_a_forbidden_import() {
    let main = format!("@import \"./lib{ESC}.mds\"\nHi\n");
    let (mut cache, n) = counting_cache(vfs(&[("main.mds", &main)]));
    let err = cache
        .resolve_key("main.mds", &HashMap::new(), &mut vec![])
        .expect_err("the resolver must refuse the import");
    assert_eq!(code_of(&err), "mds::import");
    assert!(err.to_string().contains("U+001B"), "{err}");
    assert_eq!(
        n.imports.load(Ordering::SeqCst),
        0,
        "backend must not be called"
    );

    // Positive control: a clean import does reach normalize_in_dir.
    let (mut cache, n) = counting_cache(vfs(&[
        ("main.mds", "@import \"./lib.mds\"\nHi\n"),
        ("lib.mds", "@define f():\nx\n@end\n"),
    ]));
    cache
        .resolve_key("main.mds", &HashMap::new(), &mut vec![])
        .expect("control compiles");
    assert_eq!(
        n.imports.load(Ordering::SeqCst),
        1,
        "control: counter counts"
    );
}

#[test]
fn custom_backend_never_sees_a_forbidden_base_dir() {
    let base = format!("dir{ESC}x");
    let (mut cache, n) = counting_cache(HashMap::new());
    let err = cache
        .resolve_source("Hi\n", &base, &HashMap::new(), &mut vec![])
        .expect_err("the resolver must refuse the base directory");
    assert_eq!(code_of(&err), "mds::io");
    assert!(
        err.to_string().contains(&format!(
            "base directory contains forbidden character U+001B: \"dir{}x\"",
            escaped(ESC)
        )),
        "{err}"
    );
    assert_eq!(
        n.anchors.load(Ordering::SeqCst),
        0,
        "backend must not be called"
    );

    // Positive control: a clean base directory reaches anchor_base_dir.
    let (mut cache, n) = counting_cache(HashMap::new());
    cache
        .resolve_source("Hi\n", "dir", &HashMap::new(), &mut vec![])
        .expect("control compiles");
    assert_eq!(
        n.anchors.load(Ordering::SeqCst),
        1,
        "control: counter counts"
    );
}

// ── Canonical paths: a hostile directory reached through a clean name ───────
//
// Unix-only: Windows file names cannot hold C0 controls, so the hostile directory
// these tests need cannot be created there.

#[cfg(unix)]
mod on_disk {
    use super::*;

    /// A `.mdsroot` project holding a directory named `evil<ESC>dir` with `x.mds`,
    /// a clean directory `clean` with `x.mds`, and `alias` → the hostile directory,
    /// `alias2` → the clean one.
    fn project() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join(".mdsroot"), "").unwrap();
        let hostile = root.join(format!("evil{ESC}dir"));
        let clean = root.join("clean");
        for d in [&hostile, &clean] {
            std::fs::create_dir(d).unwrap();
            std::fs::write(d.join("x.mds"), "@define f():\nx\n@end\n").unwrap();
            std::fs::write(d.join("main.mds"), "Hi\n").unwrap();
        }
        std::os::unix::fs::symlink(&hostile, root.join("alias")).unwrap();
        std::os::unix::fs::symlink(&clean, root.join("alias2")).unwrap();
        (dir, root)
    }

    #[test]
    fn import_through_a_symlinked_hostile_parent_is_refused() {
        let (_guard, root) = project();
        std::fs::write(root.join("main.mds"), "@import \"./alias/x.mds\"\nHi\n").unwrap();
        let err =
            mds::compile(root.join("main.mds"), None).expect_err("the resolved path carries ESC");
        assert_eq!(code_of(&err), "mds::io", "{err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains("resolved path contains forbidden character U+001B: \"./alias/x.mds\""),
            "{msg}"
        );
        assert!(
            !msg.contains(&*root.to_string_lossy()),
            "the absolute canonical path must not be shown: {msg}"
        );
        assert_no_forbidden(&msg, "symlinked parent");

        // Control: the same shape through a clean-named target compiles.
        std::fs::write(root.join("main.mds"), "@import \"./alias2/x.mds\"\nHi\n").unwrap();
        mds::compile(root.join("main.mds"), None).expect("control compiles");
    }

    #[test]
    fn entry_under_a_hostile_directory_is_refused() {
        let (_guard, root) = project();

        // Typed: the hostile name is in the path the caller passes.
        let typed = root.join(format!("evil{ESC}dir")).join("main.mds");
        let err = mds::compile(&typed, None).expect_err("typed hostile entry");
        assert_eq!(code_of(&err), "mds::io");
        assert!(err
            .to_string()
            .contains("entry path contains forbidden character U+001B"));

        // Through a clean symlinked parent: only the canonical path carries it.
        let via_alias = root.join("alias").join("main.mds");
        let err = mds::compile(&via_alias, None).expect_err("hostile canonical entry");
        assert_eq!(code_of(&err), "mds::io");
        let msg = err.to_string();
        assert!(
            msg.contains("resolved path contains forbidden character U+001B"),
            "{msg}"
        );
        assert_no_forbidden(&msg, "entry via alias");

        // Control: a clean symlinked parent is fine.
        mds::compile(root.join("alias2").join("main.mds"), None).expect("control compiles");
    }

    #[test]
    fn string_base_dir_symlinked_to_a_hostile_directory_is_refused() {
        let (_guard, root) = project();
        let alias = root.join("alias");
        let err = mds::compile_str_with("Hi\n", Some(&alias), None)
            .expect_err("base dir resolves into a hostile directory");
        assert_eq!(code_of(&err), "mds::io");
        let msg = err.to_string();
        assert!(
            msg.contains("resolved path contains forbidden character U+001B"),
            "{msg}"
        );
        assert!(
            msg.contains(&format!("\"{}\"", alias.display())),
            "the message shows the base directory as passed: {msg}"
        );
        assert_no_forbidden(&msg, "base dir via alias");

        // Control.
        mds::compile_str_with("Hi\n", Some(&root.join("alias2")), None).expect("control");
    }
}
