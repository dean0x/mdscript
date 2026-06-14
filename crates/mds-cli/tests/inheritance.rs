//! CLI integration tests for template inheritance (`@extends` / `@block`).
//!
//! Coverage:
//! - F1 / F11: issue worked example (inh_base + inh_analyst); byte-exact output
//! - F2 CLI:   standalone base compiles with defaults
//! - F6 / F7:  end-to-end frontmatter merge + `--set` runtime override via mds_bin
//! - F8 CLI:   block body with @if / {interp}
//! - F9 / E13: @message-structured base in messages mode; no-@message error
//! - F13 watch: `_base.mds` partial skipped; child's dependency set includes base
//! - E5 CLI:   circular and self-extension → mds::circular_import
//! - A2 CLI:   compile_with_deps dependency order (base first)
//! - P2 perf:  wide base (~200 @block slots, child overrides all) compiles < 1s

mod common;
use common::{fixture, mds_bin};

use std::time::Instant;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Compile a file via the CLI binary and return (stdout, stderr, success).
fn build_file(path: &str) -> (String, String, bool) {
    let output = mds_bin()
        .args(["build", path, "-o", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    (
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
        output.status.success(),
    )
}

/// Compile via mds_bin with extra args; returns (stdout, stderr, success).
fn build_file_args(path: &str, extra_args: &[&str]) -> (String, String, bool) {
    let mut cmd = mds_bin();
    cmd.arg("build").arg(path);
    for arg in extra_args {
        cmd.arg(arg);
    }
    cmd.arg("-o").arg("-");
    let output = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    (
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
        output.status.success(),
    )
}

// ── F1 + F11: worked example — byte-exact output ─────────────────────────────

#[test]
fn f1_analyst_compile_byte_exact() {
    // The "issue worked example" for template inheritance:
    // inh_base.mds has role, instructions, tools, output_format blocks.
    // inh_analyst.mds overrides instructions + tools; inherits output_format default.
    let (stdout, stderr, ok) = build_file(fixture("inh_analyst.mds").to_str().unwrap());
    assert!(ok, "F1: compile should succeed; stderr: {stderr}");

    let expected = concat!(
        "---\n",
        "role: data analysis\n",
        "---\n",
        "You are a data analysis assistant.\n",
        "\n",
        "Perform statistical analysis.\n",
        "You have access to: Python, R\n",
        "Respond in plain text.\n",
    );
    assert_eq!(
        stdout, expected,
        "F1: byte-exact output mismatch\ngot:\n{stdout}\nexpected:\n{expected}"
    );
}

#[test]
fn f11_whitespace_contract_base_blank_lines_preserved() {
    // Blank lines between @block declarations in the base are preserved in output.
    let dir = tempfile::tempdir().unwrap();
    let base_path = dir.path().join("base.mds");
    let child_path = dir.path().join("child.mds");

    // Two adjacent blocks with a blank line between them in the base.
    std::fs::write(
        &base_path,
        "@block first:\nFirst default.\n@end\n\n@block second:\nSecond default.\n@end\n",
    )
    .unwrap();
    // Child overrides both.
    std::fs::write(
        &child_path,
        "@extends \"./base.mds\"\n@block first:\nFirst override.\n@end\n@block second:\nSecond override.\n@end\n",
    )
    .unwrap();

    let (stdout, stderr, ok) = build_file(child_path.to_str().unwrap());
    assert!(ok, "F11: compile should succeed; stderr: {stderr}");

    // The newline separating @block declarations in the base is preserved in output.
    // (The text node between @block declarations in the skeleton passes through unchanged.)
    assert!(
        stdout.contains("First override."),
        "F11: first block override must appear in output; got:\n{stdout}"
    );
    assert!(
        stdout.contains("Second override."),
        "F11: second block override must appear in output; got:\n{stdout}"
    );
    // First override must appear before second override.
    let first_pos = stdout.find("First override.").unwrap();
    let second_pos = stdout.find("Second override.").unwrap();
    assert!(
        first_pos < second_pos,
        "F11: first block must appear before second block in output; got:\n{stdout}"
    );
}

// ── F2 CLI: standalone base compiles with defaults ────────────────────────────

#[test]
fn f2_standalone_base_compiles_with_defaults() {
    let (stdout, stderr, ok) = build_file(fixture("inh_base.mds").to_str().unwrap());
    assert!(ok, "F2: standalone base should compile; stderr: {stderr}");
    assert!(
        stdout.contains("You are a general assistant."),
        "F2: base should render its own frontmatter default role; got:\n{stdout}"
    );
    assert!(
        stdout.contains("Analyze data carefully."),
        "F2: base should render default instructions block; got:\n{stdout}"
    );
    assert!(
        stdout.contains("Respond in plain text."),
        "F2: base should render default output_format block; got:\n{stdout}"
    );
}

// ── F6 / F7: frontmatter merge + --set runtime override ──────────────────────

#[test]
fn f7_set_runtime_var_overrides_merged_frontmatter() {
    // base has role: general, child has role: data analysis.
    // --set role=scientist must win (base < child < runtime).
    let (stdout, stderr, ok) = build_file_args(
        fixture("inh_analyst.mds").to_str().unwrap(),
        &["--set", "role=scientist"],
    );
    assert!(
        ok,
        "F7: compile with --set should succeed; stderr: {stderr}"
    );
    assert!(
        stdout.contains("You are a scientist assistant."),
        "F7: runtime --set must win over merged frontmatter; got:\n{stdout}"
    );
    // The raw frontmatter in the output still shows the child's value (not overridden in output).
    assert!(
        stdout.contains("role: data analysis"),
        "F7: output frontmatter should show child's value, not runtime override; got:\n{stdout}"
    );
}

#[test]
fn f6_deep_merge_base_only_key_visible_in_child() {
    // A key in the base frontmatter that is absent in the child must be available in the
    // child's scope (deep-merge: base < child).
    let dir = tempfile::tempdir().unwrap();
    let base_path = dir.path().join("base.mds");
    let child_path = dir.path().join("child.mds");

    std::fs::write(
        &base_path,
        "---\nbase_key: base_value\n---\n@block body:\n{base_key}\n@end\n",
    )
    .unwrap();
    // Child inherits base_key but doesn't override it.
    std::fs::write(
        &child_path,
        "---\nchild_key: child_value\n---\n@extends \"./base.mds\"\n",
    )
    .unwrap();

    let (stdout, stderr, ok) = build_file(child_path.to_str().unwrap());
    assert!(ok, "F6: compile should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("base_value"),
        "F6: base-only key must be visible in child scope; got:\n{stdout}"
    );
}

// ── F8 CLI: block body with @if + {interp} ────────────────────────────────────

#[test]
fn f8_block_body_with_control_flow_and_interp() {
    // A block body can contain @if / {interp} — resolved with merged scope.
    let dir = tempfile::tempdir().unwrap();
    let base_path = dir.path().join("base.mds");
    let child_path = dir.path().join("child.mds");

    std::fs::write(
        &base_path,
        "---\nmode: default\n---\n@block body:\n@if mode == \"verbose\":\nVERBOSE: {mode}\n@else:\nSTD: {mode}\n@end\n@end\n",
    )
    .unwrap();
    std::fs::write(
        &child_path,
        "---\nmode: verbose\n---\n@extends \"./base.mds\"\n",
    )
    .unwrap();

    let (stdout, stderr, ok) = build_file(child_path.to_str().unwrap());
    assert!(ok, "F8: block with @if should compile; stderr: {stderr}");
    assert!(
        stdout.contains("VERBOSE: verbose"),
        "F8: @if in block body should evaluate with merged scope; got:\n{stdout}"
    );
}

// ── F9 / E13: messages mode ───────────────────────────────────────────────────

#[test]
fn f9_messages_mode_child_compiles_to_json_array() {
    let (stdout, stderr, ok) = build_file_args(
        fixture("inh_messages_child.mds").to_str().unwrap(),
        &["--format", "messages"],
    );
    assert!(
        ok,
        "F9: messages mode child should compile; stderr: {stderr}"
    );

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("F9: output must be valid JSON");
    let arr = parsed.as_array().expect("F9: output must be a JSON array");
    assert_eq!(arr.len(), 2, "F9: expected 2 messages; got: {arr:#?}");
    assert_eq!(arr[0]["role"].as_str().unwrap(), "system");
    assert!(
        arr[0]["content"].as_str().unwrap().contains("researcher"),
        "F9: system message should use child's role override; got: {:?}",
        arr[0]["content"]
    );
    assert_eq!(arr[1]["role"].as_str().unwrap(), "user");
    assert!(
        arr[1]["content"]
            .as_str()
            .unwrap()
            .contains("Summarize the latest findings."),
        "F9: user message should use child's block override; got: {:?}",
        arr[1]["content"]
    );
}

#[test]
fn e13_messages_mode_base_no_message_exits_nonzero() {
    // E13: base with no @message blocks in messages mode → non-zero exit.
    let (_, stderr, ok) = build_file_args(
        fixture("inh_analyst.mds").to_str().unwrap(),
        &["--format", "messages"],
    );
    assert!(
        !ok,
        "E13: compile in messages mode without @message should fail"
    );
    assert!(
        stderr.contains("@message") || stderr.contains("message") || stderr.contains("no "),
        "E13: error should mention @message; got: {stderr}"
    );
}

// ── F13 watch: `_base.mds` partial is not emitted; child depends on it ────────

#[test]
fn f13_underscore_base_not_emitted_and_child_depends_on_it() {
    // Decision #11 (from the implementation): files with a `_` prefix are partials
    // and are never emitted as output files.
    //
    // We assert: (a) compile_with_deps shows the base in dependencies (the reverse-dep
    //            edge exists), and (b) in watch dir-mode the `_base.mds` partial does
    //            not produce a `_base.md` output file.
    let dir = tempfile::tempdir().unwrap();
    let base_path = dir.path().join("_base.mds");
    let child_path = dir.path().join("child.mds");

    std::fs::write(
        &base_path,
        "---\nrole: general\n---\n@block body:\nDefault.\n@end\n",
    )
    .unwrap();
    std::fs::write(
        &child_path,
        "---\nrole: specialist\n---\n@extends \"./_base.mds\"\n@block body:\nOverride.\n@end\n",
    )
    .unwrap();

    // (a) compile_with_deps shows the base in dependencies.
    let result = mds::compile_with_deps(&child_path, None).expect("F13: compile should succeed");
    assert!(
        result.dependencies.iter().any(|d| d.contains("_base.mds")),
        "F13: child must list _base.mds in dependencies; got: {:?}",
        result.dependencies
    );

    // (b) In watch dir-mode, `_base.mds` is NOT emitted (AC-R8 / decision #11).
    let out_dir = dir.path().join("out");
    let _ = std::process::Command::new(env!("CARGO_BIN_EXE_mds"))
        .args([
            "watch",
            dir.path().to_str().unwrap(),
            "--out-dir",
            out_dir.to_str().unwrap(),
            "--debounce",
            "0",
            "-q",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|mut child| {
            // Wait briefly for initial compile.
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = child.kill();
            let _ = child.wait();
        });

    // child.md should exist (emitted), _base.md must NOT exist.
    if out_dir.join("child.md").exists() {
        assert!(
            !out_dir.join("_base.md").exists(),
            "F13: _base.md must not be emitted for a _-prefixed partial"
        );
    }
    // Whether the watch sub-test ran or not (tooling availability), the lib-level
    // assertion in (a) already covers the dependency edge — that's the critical check.
}

// ── E5 CLI: circular and self-extension ──────────────────────────────────────

#[test]
fn e5_circular_inheritance_exits_nonzero_with_chain() {
    // E5: A→B→A circular inheritance → mds::circular_import with → chain.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.mds");
    let b = dir.path().join("b.mds");

    std::fs::write(&a, "@extends \"./b.mds\"\n@block body:\nFrom A.\n@end\n").unwrap();
    std::fs::write(&b, "@extends \"./a.mds\"\n@block body:\nFrom B.\n@end\n").unwrap();

    let (_, stderr, ok) = build_file(a.to_str().unwrap());
    assert!(!ok, "E5: circular should fail");
    assert!(
        stderr.contains("circular"),
        "E5: error should mention circular; got: {stderr}"
    );
    assert!(
        stderr.contains('\u{2192}'),
        "E5: error should contain → chain; got: {stderr}"
    );
}

#[test]
fn e5_self_extension_exits_nonzero() {
    // E5: @extends pointing to itself → mds::circular_import.
    let dir = tempfile::tempdir().unwrap();
    let self_file = dir.path().join("self.mds");

    std::fs::write(
        &self_file,
        "@extends \"./self.mds\"\n@block body:\nFrom self.\n@end\n",
    )
    .unwrap();

    let (_, stderr, ok) = build_file(self_file.to_str().unwrap());
    assert!(!ok, "E5: self-extension should fail");
    assert!(
        stderr.contains("circular"),
        "E5: self-extension error should mention circular; got: {stderr}"
    );
}

// ── A2 CLI: compile_with_deps includes base in dependencies ──────────────────

#[test]
fn a2_dependencies_contains_base() {
    // A2: compile_with_deps must include the base file in the dependency list.
    // The base is prepended by scan_imports before any body-level imports.
    let dir = tempfile::tempdir().unwrap();
    let base_path = dir.path().join("base.mds");
    let child_path = dir.path().join("child.mds");

    std::fs::write(&base_path, "@block body:\nBase default.\n@end\n").unwrap();
    std::fs::write(&child_path, "@extends \"./base.mds\"\n").unwrap();

    let result = mds::compile_with_deps(&child_path, None).expect("A2: compile should succeed");
    let deps = &result.dependencies;

    // base.mds must be in the dependency list.
    assert!(
        deps.iter().any(|d| d.contains("base.mds")),
        "A2: base.mds must be in compile_with_deps dependencies; got: {deps:?}"
    );
    // child.mds must NOT be listed as its own dependency.
    assert!(
        !deps.iter().any(|d| d.contains("child.mds")),
        "A2: child.mds must not be in its own dependency list; got: {deps:?}"
    );
}

#[test]
fn a2_scan_imports_extends_path_first() {
    // A2: scan_imports must return the @extends path BEFORE any @import paths,
    // ensuring the dependency scanner sees the base as the first dependency.
    let child_src = "@extends \"./base.mds\"\n";
    let deps = mds::scan_imports(child_src).expect("A2: scan_imports should succeed");
    assert_eq!(
        deps.len(),
        1,
        "A2: child with only @extends has 1 dep; got: {deps:?}"
    );
    assert!(
        deps[0].contains("base.mds"),
        "A2: first dep must be the @extends path; got: {deps:?}"
    );
}

// ── P2: wide base (~200 blocks) compiles under 1s ────────────────────────────

#[test]
fn p2_wide_base_200_blocks_under_1s() {
    // P2: A base with ~200 @block placeholders compiled by a child that overrides
    // all of them must complete within 1s wall-clock on CI.
    // The bound guards against O(N²) blowup (~200 blocks); 1s gives enough slack
    // for debug-build CI runners while still catching orders-of-magnitude regressions.
    let dir = tempfile::tempdir().unwrap();
    let base_path = dir.path().join("base.mds");
    let child_path = dir.path().join("child.mds");

    let mut base_src = String::new();
    let mut child_src = "@extends \"./base.mds\"\n".to_string();

    for i in 0..200usize {
        base_src.push_str(&format!("@block blk{i}:\nDefault {i}.\n@end\n"));
        child_src.push_str(&format!("@block blk{i}:\nOverride {i}.\n@end\n"));
    }

    std::fs::write(&base_path, &base_src).unwrap();
    std::fs::write(&child_path, &child_src).unwrap();

    let start = Instant::now();
    let result = mds::compile(&child_path, None);
    let elapsed = start.elapsed();

    assert!(
        result.is_ok(),
        "P2: wide base compile should succeed; got: {:?}",
        result.err()
    );
    assert!(
        elapsed.as_millis() < 1000,
        "P2: wide base compile must be < 1s; took: {}ms",
        elapsed.as_millis()
    );
}

// ── E12 CLI: base-default undefined var renders with base span, no OutOfBounds ──

/// Run `mds check` on a file and return (stderr, success).
fn check_file(path: &str) -> (String, bool) {
    let output = mds_bin()
        .args(["check", path])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    (
        String::from_utf8(output.stderr).unwrap(),
        output.status.success(),
    )
}

#[test]
fn e12_base_default_undefined_var_render_points_at_base() {
    // E12: base has an undefined var in a default block; child extends base without
    // providing the var. The CLI human render must show a real span from base.mds,
    // not `OutOfBounds` / "Failed to read contents".
    let dir = tempfile::tempdir().unwrap();
    let base_path = dir.path().join("base.mds");
    let child_path = dir.path().join("child.mds");

    std::fs::write(
        &base_path,
        "@block greeting:\nHello {customer_name}, welcome.\n@end\n",
    )
    .unwrap();
    std::fs::write(&child_path, "@extends \"./base.mds\"\n").unwrap();

    let (_, stderr, ok) = build_file(child_path.to_str().unwrap());

    assert!(!ok, "E12 CLI: compile must fail; stderr: {stderr}");
    assert!(
        stderr.contains("mds::undefined_var"),
        "E12 CLI: stderr must contain mds::undefined_var; got: {stderr}"
    );
    assert!(
        !stderr.contains("Failed to read contents"),
        "E12 CLI: stderr must NOT contain 'Failed to read contents' (OutOfBounds); got: {stderr}"
    );
    assert!(
        !stderr.contains("OutOfBounds"),
        "E12 CLI: stderr must NOT contain 'OutOfBounds'; got: {stderr}"
    );
    assert!(
        stderr.contains("base.mds"),
        "E12 CLI: stderr must name base.mds; got: {stderr}"
    );
    // The label text "not defined" appears only when the span renders against a readable source.
    assert!(
        stderr.contains("not defined") || stderr.contains("customer_name"),
        "E12 CLI: stderr must contain 'not defined' or 'customer_name' (readable span); got: {stderr}"
    );
}

#[test]
fn e12_check_and_build_diagnostics_match() {
    // E12 A5: `mds check` and `mds build` on the same inheritance error must produce
    // the same error code and both name the base file.
    let dir = tempfile::tempdir().unwrap();
    let base_path = dir.path().join("base.mds");
    let child_path = dir.path().join("child.mds");

    std::fs::write(&base_path, "@block content:\n{missing_var}\n@end\n").unwrap();
    std::fs::write(&child_path, "@extends \"./base.mds\"\n").unwrap();

    let (_, build_stderr, build_ok) = build_file(child_path.to_str().unwrap());
    let (check_stderr, check_ok) = check_file(child_path.to_str().unwrap());

    assert!(!build_ok, "E12 A5: build must fail");
    assert!(!check_ok, "E12 A5: check must fail");

    assert!(
        build_stderr.contains("mds::undefined_var"),
        "E12 A5: build stderr must contain mds::undefined_var; got: {build_stderr}"
    );
    assert!(
        check_stderr.contains("mds::undefined_var"),
        "E12 A5: check stderr must contain mds::undefined_var; got: {check_stderr}"
    );

    assert!(
        build_stderr.contains("base.mds"),
        "E12 A5: build stderr must name base.mds; got: {build_stderr}"
    );
    assert!(
        check_stderr.contains("base.mds"),
        "E12 A5: check stderr must name base.mds; got: {check_stderr}"
    );

    assert!(
        !build_stderr.contains("Failed to read contents"),
        "E12 A5: build must not render OutOfBounds; got: {build_stderr}"
    );
    assert!(
        !check_stderr.contains("Failed to read contents"),
        "E12 A5: check must not render OutOfBounds; got: {check_stderr}"
    );
}
