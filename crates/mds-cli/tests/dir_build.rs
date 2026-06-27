//! Integration tests for `mds build <dir>` and `mds check <dir>`.
//!
//! Coverage:
//! - T-CLI-12 (FUNC-16): subtree mirror + intrinsic ext per file with --out-dir
//! - T-CLI-13 (FUNC-17): `_`-prefixed partials produce no output
//! - T-CLI-14 (FUNC-18): continue-on-error: per-file error, summary, non-zero exit
//! - T-CLI-15 (FUNC-19): bare build without --out-dir writes next-to-source
//! - T-CLI-16 (FUNC-20): in-tree symlinked file/dir skipped; symlinked entry root rejected
//! - T-CLI-17 (FUNC-21): one oversized (>10 MiB) file fails while others succeed
//! - T-CLI-20 (FUNC-26): `check <dir>` validates tree, continues on error, non-zero exit on failure
//! - T-CLI-21 (FUNC-16, unit): `output_path_for("json"/"md")` + `..`-containment (AC-M7)
//!   (covered in output.rs unit tests; here we test via CLI)

mod common;
use common::mds_bin;

use std::fs;
use std::path::Path;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn create_plain_mds(dir: &Path, name: &str) {
    // Use no template variables so this always compiles successfully.
    fs::write(dir.join(name), "Hello, world!\n").unwrap();
}

fn create_messages_mds(dir: &Path, name: &str) {
    fs::write(
        dir.join(name),
        "@message system:\nYou are a helpful assistant.\n@end\n@message user:\nHello!\n@end\n",
    )
    .unwrap();
}

/// A syntactically invalid .mds file (undefined variable causes a compile error).
fn create_bad_mds(dir: &Path, name: &str) {
    fs::write(dir.join(name), "{undefined_var_xyz}\n").unwrap();
}

fn build_dir(dir: &Path, extra_args: &[&str]) -> std::process::Output {
    mds_bin()
        .arg("build")
        .arg(dir)
        .args(extra_args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap()
}

fn check_dir(dir: &Path, extra_args: &[&str]) -> std::process::Output {
    mds_bin()
        .arg("check")
        .arg(dir)
        .args(extra_args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap()
}

// ── T-CLI-12 (FUNC-16): subtree mirror + intrinsic ext per file ───────────────

#[test]
fn dir_build_subtree_mirror_intrinsic_ext() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    // Structure: src/plain.mds, src/sub/messages.mds
    fs::create_dir(src.path().join("sub")).unwrap();
    create_plain_mds(src.path(), "plain.mds");
    create_messages_mds(&src.path().join("sub"), "messages.mds");

    let output = build_dir(src.path(), &["--out-dir", out.path().to_str().unwrap()]);

    assert!(
        output.status.success(),
        "dir build should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // plain.mds → out/plain.md (markdown kind)
    let plain_out = out.path().join("plain.md");
    assert!(
        plain_out.exists(),
        "expected out/plain.md to be created; out dir: {:?}",
        fs::read_dir(out.path()).map(|rd| rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect::<Vec<_>>())
    );
    let plain_content = fs::read_to_string(&plain_out).unwrap();
    assert!(
        plain_content.contains("Hello"),
        "plain.md should contain rendered content; got: {plain_content:?}"
    );

    // sub/messages.mds → out/sub/messages.json (messages kind)
    let msg_out = out.path().join("sub").join("messages.json");
    assert!(
        msg_out.exists(),
        "expected out/sub/messages.json to be created"
    );
    let msg_content = fs::read_to_string(&msg_out).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&msg_content).expect("messages.json should be valid JSON");
    assert!(parsed.is_array(), "messages.json should be a JSON array");

    // Summary mentions built count
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("built"),
        "stderr should contain build summary; got: {stderr}"
    );
}

// ── T-CLI-13 (FUNC-17): `_`-prefixed partials produce no output ──────────────

#[test]
fn dir_build_partials_produce_no_output() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    // _partial.mds starts with `_` — should be skipped.
    create_plain_mds(src.path(), "_partial.mds");
    // main.mds is a normal file.
    create_plain_mds(src.path(), "main.mds");

    let output = build_dir(src.path(), &["--out-dir", out.path().to_str().unwrap()]);

    assert!(
        output.status.success(),
        "dir build should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // _partial.mds must NOT produce _partial.md
    let partial_out = out.path().join("_partial.md");
    assert!(
        !partial_out.exists(),
        "_partial.md must not be created (partials are skipped)"
    );

    // main.mds MUST produce main.md
    let main_out = out.path().join("main.md");
    assert!(main_out.exists(), "main.md should be created");
}

// ── T-CLI-14 (FUNC-18): continue-on-error, summary, non-zero exit ────────────

#[test]
fn dir_build_continue_on_error_summary_nonzero() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    // good.mds compiles fine; bad.mds has an undefined variable.
    create_plain_mds(src.path(), "good.mds");
    create_bad_mds(src.path(), "bad.mds");

    let output = build_dir(src.path(), &["--out-dir", out.path().to_str().unwrap()]);

    // Must exit non-zero because bad.mds failed.
    assert!(
        !output.status.success(),
        "dir build with a failing file must exit non-zero"
    );

    // good.mds MUST still be written (continue-on-error).
    let good_out = out.path().join("good.md");
    assert!(
        good_out.exists(),
        "good.md should still be created despite bad.mds failing"
    );

    // bad.mds should NOT produce output.
    let bad_out = out.path().join("bad.md");
    assert!(
        !bad_out.exists(),
        "bad.md must not be created when compilation fails"
    );

    // Summary must be present in stderr.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("built") && stderr.contains("failed"),
        "stderr must contain a build summary with 'built' and 'failed'; got: {stderr}"
    );
    // Specifically: "1 built, 1 failed"
    assert!(
        stderr.contains("1 built") && stderr.contains("1 failed"),
        "stderr must show '1 built, 1 failed'; got: {stderr}"
    );
}

// ── T-CLI-15 (FUNC-19): bare build writes next-to-source ─────────────────────

#[test]
fn dir_build_bare_writes_next_to_source() {
    let src = tempfile::tempdir().unwrap();

    create_plain_mds(src.path(), "page.mds");
    create_messages_mds(src.path(), "chat.mds");

    // No --out-dir: outputs go next to source.
    let output = build_dir(src.path(), &[]);

    assert!(
        output.status.success(),
        "bare dir build should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // plain → same dir, .md
    let page_out = src.path().join("page.md");
    assert!(page_out.exists(), "page.md should appear next to page.mds");

    // messages → same dir, .json
    let chat_out = src.path().join("chat.json");
    assert!(
        chat_out.exists(),
        "chat.json should appear next to chat.mds"
    );

    // Clean up outputs.
    let _ = fs::remove_file(&page_out);
    let _ = fs::remove_file(&chat_out);
}

// ── T-CLI-16 (FUNC-20): in-tree symlinks skipped; symlinked entry root rejected

#[cfg(unix)]
#[test]
fn dir_build_symlinked_file_skipped() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    // Create a real file and a symlink to it.
    let real_file = src.path().join("real.mds");
    fs::write(&real_file, "Real content.\n").unwrap();
    let link_file = src.path().join("link.mds");
    std::os::unix::fs::symlink(&real_file, &link_file).unwrap();

    let output = build_dir(src.path(), &["--out-dir", out.path().to_str().unwrap()]);

    assert!(
        output.status.success(),
        "build should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The symlinked file (link.mds) must NOT produce output.
    let link_out = out.path().join("link.md");
    assert!(
        !link_out.exists(),
        "symlinked .mds file must not produce output"
    );

    // The real file MUST produce output.
    let real_out = out.path().join("real.md");
    assert!(real_out.exists(), "real.md should be created from real.mds");
}

#[cfg(unix)]
#[test]
fn dir_build_symlinked_subdir_skipped() {
    let real_dir = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    // Place a .mds file in real_dir and make a symlink into src pointing at it.
    fs::write(real_dir.path().join("child.mds"), "Child.\n").unwrap();
    let link_dir = src.path().join("linked_sub");
    std::os::unix::fs::symlink(real_dir.path(), &link_dir).unwrap();

    // Also create a real file at the root so the build isn't empty.
    create_plain_mds(src.path(), "root.mds");

    let output = build_dir(src.path(), &["--out-dir", out.path().to_str().unwrap()]);

    assert!(
        output.status.success(),
        "build should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The symlinked directory must not be traversed — child.mds in it produces no output.
    let child_out = out.path().join("linked_sub").join("child.md");
    assert!(
        !child_out.exists(),
        "symlinked subdirectory must not be traversed"
    );
}

#[cfg(unix)]
#[test]
fn dir_build_symlinked_entry_root_rejected() {
    let real_dir = tempfile::tempdir().unwrap();
    create_plain_mds(real_dir.path(), "page.mds");

    // Create a symlink pointing at the real dir.
    let link_dir = tempfile::tempdir().unwrap();
    let link_path = link_dir.path().join("linked");
    std::os::unix::fs::symlink(real_dir.path(), &link_path).unwrap();

    let output = build_dir(&link_path, &[]);

    assert!(
        !output.status.success(),
        "build on a symlinked entry root must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("symlink") || stderr.contains("directory"),
        "error must mention symlink; got: {stderr}"
    );
}

// ── T-CLI-17 (FUNC-21): oversized file fails, others succeed ─────────────────

#[test]
fn dir_build_oversized_file_fails_others_succeed() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    // Create a good file.
    create_plain_mds(src.path(), "good.mds");

    // Create an oversized file (>10 MiB = > 10 * 1024 * 1024 bytes).
    let big_content = "x".repeat(10 * 1024 * 1024 + 1);
    fs::write(src.path().join("big.mds"), &big_content).unwrap();

    let output = build_dir(src.path(), &["--out-dir", out.path().to_str().unwrap()]);

    // Must exit non-zero because big.mds failed the size cap.
    assert!(
        !output.status.success(),
        "dir build with an oversized file must exit non-zero"
    );

    // good.mds MUST still be written.
    let good_out = out.path().join("good.md");
    assert!(
        good_out.exists(),
        "good.md should still be created despite the oversized file"
    );

    // Summary must mention the failure.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed"),
        "stderr must mention failure in summary; got: {stderr}"
    );
}

// ── T-CLI-20 (FUNC-26): `check <dir>` ────────────────────────────────────────

#[test]
fn dir_check_validates_tree_exits_zero_on_all_ok() {
    let src = tempfile::tempdir().unwrap();

    create_plain_mds(src.path(), "page.mds");
    create_messages_mds(src.path(), "chat.mds");

    let output = check_dir(src.path(), &[]);

    assert!(
        output.status.success(),
        "check on a valid tree should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("checked"),
        "stderr should contain check summary; got: {stderr}"
    );
}

#[test]
fn dir_check_continues_on_error_nonzero_exit() {
    let src = tempfile::tempdir().unwrap();

    create_plain_mds(src.path(), "good.mds");
    create_bad_mds(src.path(), "bad.mds");

    let output = check_dir(src.path(), &[]);

    // Must exit non-zero because bad.mds failed.
    assert!(
        !output.status.success(),
        "check with a failing file must exit non-zero"
    );

    // Summary should show counts.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("checked") || stderr.contains("failed"),
        "stderr must contain check summary; got: {stderr}"
    );
}

#[test]
fn dir_check_empty_dir_exits_zero() {
    let src = tempfile::tempdir().unwrap();

    let output = check_dir(src.path(), &[]);

    assert!(
        output.status.success(),
        "check on empty dir should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No .mds files") || stderr.contains("no .mds"),
        "stderr should mention no files found; got: {stderr}"
    );
}

#[test]
fn dir_check_mixed_content_file_nonzero() {
    let src = tempfile::tempdir().unwrap();

    // Mixed content: text before @message — AC-FUNC-25 / mds::mixed_content error.
    fs::write(
        src.path().join("mixed.mds"),
        "Some text before.\n@message user:\nHello!\n@end\n",
    )
    .unwrap();

    let output = check_dir(src.path(), &[]);

    assert!(
        !output.status.success(),
        "check on a mixed-content file must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("mixed") || stderr.contains("failed"),
        "error must mention mixed content or failure; got: {stderr}"
    );
}

// ── T-CLI-21 variant: stale-output cleanup on format-flip (dir build) ─────────

#[test]
fn dir_build_stale_output_cleaned_on_format_flip() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    // Step 1: Build a plain template → produces page.md
    let mds_path = src.path().join("page.mds");
    fs::write(&mds_path, "Hello, world!\n").unwrap();

    let output1 = build_dir(src.path(), &["--out-dir", out.path().to_str().unwrap()]);
    assert!(
        output1.status.success(),
        "first build should succeed; stderr: {}",
        String::from_utf8_lossy(&output1.stderr)
    );
    let md_out = out.path().join("page.md");
    assert!(md_out.exists(), "page.md should be created on first build");

    // Step 2: Rewrite the template to use @message → would produce page.json
    fs::write(&mds_path, "@message user:\nHello!\n@end\n").unwrap();

    let output2 = build_dir(src.path(), &["--out-dir", out.path().to_str().unwrap()]);
    assert!(
        output2.status.success(),
        "second build should succeed; stderr: {}",
        String::from_utf8_lossy(&output2.stderr)
    );
    let json_out = out.path().join("page.json");
    assert!(
        json_out.exists(),
        "page.json should be created on second build"
    );

    // Stale page.md from step 1 must be removed.
    assert!(
        !md_out.exists(),
        "stale page.md should be removed after format flip to messages"
    );
}

// ── T-CLI: empty dir build exits zero ────────────────────────────────────────

#[test]
fn dir_build_empty_dir_exits_zero() {
    let src = tempfile::tempdir().unwrap();

    let output = build_dir(src.path(), &[]);

    assert!(
        output.status.success(),
        "build on empty dir should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No .mds files") || stderr.contains("no .mds"),
        "stderr should mention no .mds files; got: {stderr}"
    );
}
