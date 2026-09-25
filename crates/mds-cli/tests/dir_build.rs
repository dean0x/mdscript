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
//! - AC-Q01 (#216): `mds build --quiet <dir>` on all-success tree produces no stderr
//! - AC-Q02 (#216): `mds build --quiet <dir>` with a failing file still prints summary
//! - AC-Q30 (#216): `--quiet` before subcommand and after produce identical results

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
    fs::write(dir.join(name), "{{undefined_var_xyz}}\n").unwrap();
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

/// Run `mds build <file>` on a single file. Same shape as [`build_dir`]; a separate
/// helper so call sites stay honest about which input form is under test.
///
/// `#[cfg(unix)]`: its only caller, `build_non_utf8_path_exits_2_and_writes_no_map`,
/// is itself Unix-only (constructs a non-UTF-8 filename via `OsStringExt`).
#[cfg(unix)]
fn build_file(path: &Path, extra_args: &[&str]) -> std::process::Output {
    mds_bin()
        .arg("build")
        .arg(path)
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
        stderr.contains("passed"),
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
        stderr.contains("passed") || stderr.contains("failed"),
        "stderr must contain check summary; got: {stderr}"
    );
}

/// #204 reversal: until v0.4.3 this test was `dir_check_empty_dir_exits_zero` and
/// pinned exit 0. `mds check` now mirrors `mds build`: an empty tree is "nothing
/// was checked", exit 1.
#[test]
fn dir_check_empty_dir_exits_one() {
    let src = tempfile::tempdir().unwrap();

    let output = check_dir(src.path(), &[]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "check on an empty dir must exit 1; stderr: {stderr}"
    );
    assert!(
        stderr.contains("no .mds files found in"),
        "stderr must carry the empty-tree diagnostic; got: {stderr:?}"
    );
    assert!(
        stderr.contains("nothing was checked"),
        "stderr must say nothing was checked; got: {stderr:?}"
    );
}

/// #204: `--quiet` must not suppress `mds check`'s empty-tree diagnostic.
#[test]
fn dir_check_empty_dir_quiet_still_emits_diagnostic() {
    let src = tempfile::tempdir().unwrap();

    let output = check_dir(src.path(), &["--quiet"]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "check --quiet on an empty dir must exit 1; stderr: {stderr}"
    );
    assert!(
        !stderr.is_empty(),
        "stderr must not be empty under --quiet on an empty tree"
    );
    assert!(
        stderr.contains("no .mds files found in"),
        "empty-tree diagnostic must appear under --quiet; got: {stderr:?}"
    );
    assert!(
        stderr.contains("nothing was checked"),
        "stderr must say nothing was checked under --quiet; got: {stderr:?}"
    );
}

/// #387: `mds check` mirrors `mds build` — a tree whose only `.mds` files are
/// partials is "nothing to check". Positive control in the same test: adding a
/// non-partial file flips the tree back to a normal successful check.
#[test]
fn dir_check_partials_only_exits_one() {
    let src = tempfile::tempdir().unwrap();

    create_plain_mds(src.path(), "_only.mds");

    let output = check_dir(src.path(), &[]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "check on a partials-only dir must exit 1; stderr: {stderr}"
    );
    assert!(
        stderr.contains("nothing was checked"),
        "stderr must say nothing was checked; got: {stderr:?}"
    );
    assert!(
        stderr.contains("all are _-prefixed partials"),
        "stderr must say all are _-prefixed partials; got: {stderr:?}"
    );

    // Positive control: a non-partial file in the same tree flips this back to a
    // normal successful check.
    create_plain_mds(src.path(), "real.mds");

    let output2 = check_dir(src.path(), &[]);

    let stderr2 = String::from_utf8_lossy(&output2.stderr);
    assert_eq!(
        output2.status.code(),
        Some(0),
        "check with a real file present must succeed; stderr: {stderr2}"
    );
    assert!(
        stderr2.contains("1 passed, 0 failed"),
        "stderr must show the check summary; got: {stderr2:?}"
    );
}

/// #204 boundary pin: a path that does NOT exist is not a directory, so it takes
/// the single-file path and exits 2 (`mds::file_not_found`) — unchanged by #204.
/// GREEN both before and after the fix; it exists to prove the new exit-1 arm
/// did not swallow the missing-path case.
#[test]
fn dir_check_missing_root_exits_two() {
    let src = tempfile::tempdir().unwrap();
    let missing = src.path().join("nope");

    let output = check_dir(&missing, &[]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "check on a missing path must exit 2; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("no .mds files found in"),
        "a missing path must NOT produce the empty-tree diagnostic; got: {stderr:?}"
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

// ── T-CLI: empty dir build exits one (#204) ──────────────────────────────────

/// #204 reversal: until v0.4.3 this test was `dir_build_empty_dir_exits_zero` and
/// pinned exit 0 on an empty tree. "Nothing to build" is an error, exactly as the
/// all-excluded case already was — a silent green pass on a mistyped or
/// not-yet-populated directory is the CI failure mode #204 closes.
#[test]
fn dir_build_empty_dir_exits_one() {
    let src = tempfile::tempdir().unwrap();

    let output = build_dir(src.path(), &[]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "build on an empty dir must exit 1; stderr: {stderr}"
    );
    assert!(
        stderr.contains("no .mds files found in"),
        "stderr must carry the empty-tree diagnostic; got: {stderr:?}"
    );
    assert!(
        stderr.contains("nothing was built"),
        "stderr must say nothing was built; got: {stderr:?}"
    );
}

/// #204: `--quiet` must not suppress the empty-tree diagnostic — this is the exact
/// CI invocation where a silent green pass is the danger. Mirrors
/// `dir_build_all_excluded_quiet_still_emits_diagnostic`.
#[test]
fn dir_build_empty_dir_quiet_still_emits_diagnostic() {
    let src = tempfile::tempdir().unwrap();

    let output = build_dir(src.path(), &["--quiet"]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "build --quiet on an empty dir must exit 1; stderr: {stderr}"
    );
    assert!(
        !stderr.is_empty(),
        "stderr must not be empty under --quiet on an empty tree"
    );
    assert!(
        stderr.contains("no .mds files found in"),
        "empty-tree diagnostic must appear under --quiet; got: {stderr:?}"
    );
    assert!(
        stderr.contains("nothing was built"),
        "stderr must say nothing was built under --quiet; got: {stderr:?}"
    );
}

// ── #387: partials-only directory is "nothing to build" ──────────────────────

/// #387: a tree whose only `.mds` files are `_`-prefixed partials is "nothing to
/// build" too — the loop skips every partial (T-CLI-13), so without this arm the
/// run silently ends `0 built, 0 failed`, exit 0: the same silent green pass #204
/// closed for the empty tree. Positive control in the same test: adding a
/// non-partial file to the tree flips the run back to a normal successful build.
#[test]
fn dir_build_partials_only_exits_one() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    create_plain_mds(src.path(), "_only.mds");

    let output = build_dir(src.path(), &["--out-dir", out.path().to_str().unwrap()]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "build on a partials-only dir must exit 1; stderr: {stderr}"
    );
    assert!(
        stderr.contains("1 .mds file(s) found in"),
        "stderr must carry the partials-only count diagnostic; got: {stderr:?}"
    );
    assert!(
        stderr.contains("all are _-prefixed partials"),
        "stderr must say all are _-prefixed partials; got: {stderr:?}"
    );
    assert!(
        stderr.contains("nothing was built"),
        "stderr must say nothing was built; got: {stderr:?}"
    );
    assert!(
        !out.path().join("_only.md").exists(),
        "a partial must never produce output"
    );

    // Positive control: a non-partial file in the same tree flips this back to a
    // normal successful build — proves the exit-1 arm fires on "all partials",
    // not on "any partial present".
    create_plain_mds(src.path(), "real.mds");

    let output2 = build_dir(src.path(), &["--out-dir", out.path().to_str().unwrap()]);

    assert_eq!(
        output2.status.code(),
        Some(0),
        "build with a real file present must succeed; stderr: {}",
        String::from_utf8_lossy(&output2.stderr)
    );
    assert!(
        out.path().join("real.md").exists(),
        "real.md should be created"
    );
    assert!(
        !out.path().join("_only.md").exists(),
        "_only.md must not be created (partials are skipped)"
    );
}

/// #387: `--quiet` must not suppress the partials-only diagnostic — mirrors the
/// empty-tree and all-excluded `--quiet` twins above.
#[test]
fn dir_build_partials_only_quiet_still_emits_diagnostic() {
    let src = tempfile::tempdir().unwrap();

    create_plain_mds(src.path(), "_only.mds");

    let output = build_dir(src.path(), &["--quiet"]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "build --quiet on a partials-only dir must exit 1; stderr: {stderr}"
    );
    assert!(
        !stderr.is_empty(),
        "stderr must not be empty under --quiet on a partials-only tree"
    );
    assert!(
        stderr.contains("nothing was built"),
        "stderr must say nothing was built under --quiet; got: {stderr:?}"
    );
}

// ── Additional test helpers for lint/fmt subcommands ─────────────────────────

fn lint_dir(dir: &Path, extra_args: &[&str]) -> std::process::Output {
    mds_bin()
        .arg("lint")
        .arg(dir)
        .args(extra_args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap()
}

fn fmt_dir(dir: &Path, extra_args: &[&str]) -> std::process::Output {
    mds_bin()
        .arg("fmt")
        .arg(dir)
        .args(extra_args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap()
}

// ── T-CLI-ALL-EXCLUDED: all candidates in excluded dirs exits non-zero ────────
//
// Covers issue #2: when every .mds file is under a default-excluded directory
// (hidden dir or node_modules), the subcommand must:
//   (a) exit non-zero, not 0 — distinguishing from a genuinely empty tree
//   (b) emit a distinct message carrying the skip count
//   (c) emit the message even under --quiet (the CI guard case)

#[test]
fn dir_build_all_excluded_exits_nonzero() {
    let src = tempfile::tempdir().unwrap();

    // .github/prompts/ is a prime template location for prompt-template compilers.
    let hidden = src.path().join(".github");
    fs::create_dir_all(hidden.join("prompts")).unwrap();
    fs::write(hidden.join("prompts").join("system.mds"), "Hello!\n").unwrap();

    let output = build_dir(src.path(), &[]);

    assert!(
        !output.status.success(),
        "build with all candidates in excluded dirs must exit non-zero; exit: {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Message must be distinct from "No .mds files found" and carry the skip count.
    assert!(
        stderr.contains("excluded") || stderr.contains("default-excluded"),
        "stderr must mention excluded directories; got: {stderr}"
    );
    assert!(
        stderr.contains('1'),
        "stderr must carry the skip count (1 file excluded); got: {stderr}"
    );
}

#[test]
fn dir_build_all_excluded_quiet_still_emits_diagnostic() {
    let src = tempfile::tempdir().unwrap();

    let nm = src.path().join("node_modules");
    fs::create_dir(&nm).unwrap();
    fs::write(nm.join("lib.mds"), "Hello!\n").unwrap();
    fs::write(nm.join("other.mds"), "World!\n").unwrap();

    // --quiet must NOT suppress the all-excluded diagnostic — this is the exact
    // CI invocation where a silent green pass is the danger.
    let output = build_dir(src.path(), &["--quiet"]);

    assert!(
        !output.status.success(),
        "build with all candidates excluded must exit non-zero even under --quiet"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.is_empty(),
        "stderr must not be empty under --quiet when all candidates are excluded"
    );
    assert!(
        stderr.contains("excluded") || stderr.contains("default-excluded"),
        "all-excluded diagnostic must appear under --quiet; got: {stderr}"
    );
    // Skip count must appear in message.
    assert!(
        stderr.contains('2'),
        "stderr must report 2 skipped files; got: {stderr}"
    );
}

// #204 reversal: until v0.4.3 this test pinned "genuinely empty dir exits 0 — only
// the all-excluded case exits non-zero". Both now exit 1; what this test still
// guards is that the two cases stay DISTINGUISHABLE by message: an empty tree says
// "no .mds files found in …", never the excluded-directories diagnostic (and vice
// versa — inline positive control below).
#[test]
fn dir_build_genuinely_empty_exits_one_without_excluded_diagnostic() {
    let src = tempfile::tempdir().unwrap();

    let output = build_dir(src.path(), &[]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "genuinely empty dir must exit 1; stderr: {stderr}"
    );
    assert!(
        stderr.contains("no .mds files found in"),
        "empty-dir message must be the empty-tree diagnostic; got: {stderr:?}"
    );
    assert!(
        !stderr.contains("excluded"),
        "empty-dir must NOT show the excluded diagnostic; got: {stderr:?}"
    );

    // Positive control: the sibling all-excluded scenario exits 1 too,
    // so the exit code alone cannot tell them apart — the message must, and the
    // control proves "excluded" is really the substring that appears there.
    let control = tempfile::tempdir().unwrap();
    let hidden = control.path().join(".github");
    fs::create_dir(&hidden).unwrap();
    fs::write(hidden.join("x.mds"), "Hello!\n").unwrap();

    let control_out = build_dir(control.path(), &[]);
    let control_stderr = String::from_utf8_lossy(&control_out.stderr);
    assert_eq!(
        control_out.status.code(),
        Some(1),
        "positive control: the all-excluded case also exits 1; stderr: {control_stderr}"
    );
    assert!(
        control_stderr.contains("excluded"),
        "positive control: the all-excluded case must carry the excluded diagnostic; \
         got: {control_stderr:?}"
    );
    assert!(
        !control_stderr.contains("no .mds files found in"),
        "positive control: the all-excluded case must NOT carry the empty-tree \
         diagnostic; got: {control_stderr:?}"
    );
}

#[test]
fn dir_build_mixed_excluded_and_normal_processes_normal() {
    // Non-excluded files must still be processed even when some are in excluded dirs.
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    // Normal file — must be built.
    create_plain_mds(src.path(), "normal.mds");

    // Excluded file — must be skipped.
    let hidden = src.path().join(".prompts");
    fs::create_dir(&hidden).unwrap();
    fs::write(hidden.join("excluded.mds"), "Hello!\n").unwrap();

    let output = build_dir(src.path(), &["--out-dir", out.path().to_str().unwrap()]);

    // The build must succeed since normal.mds was found and processed.
    assert!(
        output.status.success(),
        "build with mixed excluded+normal files must succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // normal.mds → out/normal.md
    assert!(
        out.path().join("normal.md").exists(),
        "normal.md must be built"
    );

    // .prompts/excluded.md must NOT exist (excluded subdir was skipped).
    assert!(
        !out.path().join(".prompts").join("excluded.md").exists(),
        "excluded.md must not be built"
    );
}

#[test]
fn dir_check_all_excluded_exits_nonzero() {
    let src = tempfile::tempdir().unwrap();

    let hidden = src.path().join(".claude");
    fs::create_dir(&hidden).unwrap();
    fs::write(hidden.join("prompt.mds"), "Hello!\n").unwrap();

    let output = check_dir(src.path(), &[]);

    assert!(
        !output.status.success(),
        "check with all candidates in excluded dirs must exit non-zero; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("excluded") || stderr.contains("default-excluded"),
        "stderr must mention excluded directories; got: {stderr}"
    );
    assert!(
        stderr.contains('1'),
        "stderr must carry the skip count; got: {stderr}"
    );
}

#[test]
fn dir_check_all_excluded_quiet_still_emits_diagnostic() {
    let src = tempfile::tempdir().unwrap();

    let hidden = src.path().join(".cursor");
    fs::create_dir_all(hidden.join("rules")).unwrap();
    fs::write(hidden.join("rules").join("rule.mds"), "Hello!\n").unwrap();

    let output = check_dir(src.path(), &["--quiet"]);

    assert!(
        !output.status.success(),
        "check with all candidates excluded must exit non-zero even under --quiet"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.is_empty() && (stderr.contains("excluded") || stderr.contains("default-excluded")),
        "all-excluded diagnostic must appear under --quiet; got: {stderr:?}"
    );
}

#[test]
fn dir_lint_all_excluded_exits_nonzero() {
    let src = tempfile::tempdir().unwrap();

    let nm = src.path().join("node_modules");
    fs::create_dir(&nm).unwrap();
    fs::write(nm.join("template.mds"), "Hello!\n").unwrap();

    let output = lint_dir(src.path(), &[]);

    assert!(
        !output.status.success(),
        "lint with all candidates in excluded dirs must exit non-zero; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("excluded") || stderr.contains("default-excluded"),
        "stderr must mention excluded directories; got: {stderr}"
    );
}

#[test]
fn dir_lint_all_excluded_quiet_still_emits_diagnostic() {
    let src = tempfile::tempdir().unwrap();

    let hidden = src.path().join(".github");
    fs::create_dir(&hidden).unwrap();
    fs::write(hidden.join("workflow.mds"), "Hello!\n").unwrap();

    let output = lint_dir(src.path(), &["--quiet"]);

    assert!(
        !output.status.success(),
        "lint with all candidates excluded must exit non-zero even under --quiet"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.is_empty() && (stderr.contains("excluded") || stderr.contains("default-excluded")),
        "all-excluded diagnostic must appear under --quiet; got: {stderr:?}"
    );
}

#[test]
fn dir_fmt_all_excluded_exits_nonzero() {
    let src = tempfile::tempdir().unwrap();

    let hidden = src.path().join(".prompts");
    fs::create_dir(&hidden).unwrap();
    fs::write(hidden.join("system.mds"), "Hello!\n").unwrap();

    let output = fmt_dir(src.path(), &[]);

    assert!(
        !output.status.success(),
        "fmt with all candidates in excluded dirs must exit non-zero; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("excluded") || stderr.contains("default-excluded"),
        "stderr must mention excluded directories; got: {stderr}"
    );
}

#[test]
fn dir_fmt_all_excluded_quiet_still_emits_diagnostic() {
    let src = tempfile::tempdir().unwrap();

    let nm = src.path().join("node_modules");
    fs::create_dir(&nm).unwrap();
    fs::write(nm.join("component.mds"), "Hello!\n").unwrap();

    let output = fmt_dir(src.path(), &["--quiet"]);

    assert!(
        !output.status.success(),
        "fmt with all candidates excluded must exit non-zero even under --quiet"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.is_empty() && (stderr.contains("excluded") || stderr.contains("default-excluded")),
        "all-excluded diagnostic must appear under --quiet; got: {stderr:?}"
    );
}

// ── #204: empty directory errors on fmt too ──────────────────────────────────

/// #204: `mds fmt <empty dir>` exits 1 — "nothing was formatted" is an error, the
/// same shape as `mds build` / `mds check` and as fmt's own all-excluded arm.
#[test]
fn dir_fmt_empty_dir_exits_one() {
    let src = tempfile::tempdir().unwrap();

    let output = fmt_dir(src.path(), &[]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "fmt on an empty dir must exit 1; stderr: {stderr}"
    );
    assert!(
        stderr.contains("no .mds files found in"),
        "stderr must carry the empty-tree diagnostic; got: {stderr:?}"
    );
    assert!(
        stderr.contains("nothing was formatted"),
        "stderr must say nothing was formatted; got: {stderr:?}"
    );
}

/// #204: `--quiet` must not suppress `mds fmt`'s empty-tree diagnostic.
#[test]
fn dir_fmt_empty_dir_quiet_still_emits_diagnostic() {
    let src = tempfile::tempdir().unwrap();

    let output = fmt_dir(src.path(), &["--quiet"]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "fmt --quiet on an empty dir must exit 1; stderr: {stderr}"
    );
    assert!(
        !stderr.is_empty(),
        "stderr must not be empty under --quiet on an empty tree"
    );
    assert!(
        stderr.contains("no .mds files found in"),
        "empty-tree diagnostic must appear under --quiet; got: {stderr:?}"
    );
    assert!(
        stderr.contains("nothing was formatted"),
        "stderr must say nothing was formatted under --quiet; got: {stderr:?}"
    );
}

/// #204: the empty-tree arm sits BEFORE fmt's read-only split (`check || diff`),
/// so `--check` behaves identically to a plain `fmt` on an empty tree.
#[test]
fn dir_fmt_empty_dir_check_flag_exits_one() {
    let src = tempfile::tempdir().unwrap();

    let output = fmt_dir(src.path(), &["--check"]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "fmt --check on an empty dir must exit 1; stderr: {stderr}"
    );
    assert!(
        stderr.contains("no .mds files found in"),
        "stderr must carry the empty-tree diagnostic under --check; got: {stderr:?}"
    );
    assert!(
        stderr.contains("nothing was formatted"),
        "stderr must say nothing was formatted under --check; got: {stderr:?}"
    );
}

// ── #387 pins: fmt/lint are unaffected — they operate on partials ────────────

/// #387 pin: `mds fmt` iterates every file including partials (T-CLI-13 does not
/// apply to fmt/lint), so a partials-only tree is real formatting work, not
/// "nothing to do". Must NOT regress into the build/check nothing-to-do wording.
#[test]
fn dir_fmt_partials_only_still_formats() {
    let src = tempfile::tempdir().unwrap();

    create_plain_mds(src.path(), "_only.mds");

    let output = fmt_dir(src.path(), &[]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "fmt on a partials-only dir must still succeed; stderr: {stderr}"
    );
    assert!(
        stderr.contains("1 unchanged"),
        "stderr must show the fmt summary; got: {stderr:?}"
    );
    assert!(
        !stderr.contains("nothing was formatted"),
        "fmt must not treat a partials-only tree as nothing-to-do; got: {stderr:?}"
    );
}

/// #387 pin: `mds lint` iterates every file including partials, so a
/// partials-only tree is real lint work, not "nothing to do". Must NOT regress
/// into the build/check nothing-to-do wording.
#[test]
fn dir_lint_partials_only_still_lints() {
    let src = tempfile::tempdir().unwrap();

    create_plain_mds(src.path(), "_only.mds");

    let output = lint_dir(src.path(), &[]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "lint on a partials-only dir must still succeed; stderr: {stderr}"
    );
    assert!(
        stderr.contains("1 clean"),
        "stderr must show the lint summary; got: {stderr:?}"
    );
    assert!(
        !stderr.contains("nothing was linted"),
        "lint must not treat a partials-only tree as nothing-to-do; got: {stderr:?}"
    );
}

// ── AC-Q01: quiet build on all-success tree produces no stderr ────────────────
//
// Positive control: the identical tree without --quiet MUST still print the summary,
// proving the absence assertion under --quiet cannot pass vacuously.
// (PF-013/ADR-009: every absence assertion needs a paired positive control.)
//
// Scope: depth <= 64, no pre-existing stale sibling outputs (AC-Q05: two ungated
// warning writers in output.rs fire above MAX_DEPTH=64 or on a stale-unlink failure;
// documentation words those cases as "no output on a *successful* build", not absolute
// silence — see run_build_directory doc block).

#[test]
fn dir_build_quiet_suppresses_summary_on_success() {
    // Both the quiet run and the positive control run need their own tempdir so
    // build outputs from the first run do not affect the second.
    let src1 = tempfile::tempdir().unwrap();
    create_plain_mds(src1.path(), "a.mds");
    create_plain_mds(src1.path(), "b.mds");

    let quiet_out = build_dir(src1.path(), &["--quiet"]);

    assert_eq!(
        quiet_out.status.code(),
        Some(0),
        "quiet build over an all-success tree must exit 0; stderr: {}",
        String::from_utf8_lossy(&quiet_out.stderr)
    );
    let quiet_stderr = String::from_utf8_lossy(&quiet_out.stderr);
    assert!(
        quiet_stderr.is_empty(),
        "AC-Q01: --quiet build with no failures must produce empty stderr; got: {quiet_stderr:?}"
    );

    // Positive control: without --quiet the summary MUST appear.
    // (Proves the `is_empty` assertion above is not vacuously passing on a broken check.)
    let src2 = tempfile::tempdir().unwrap();
    create_plain_mds(src2.path(), "a.mds");
    create_plain_mds(src2.path(), "b.mds");

    let control_out = build_dir(src2.path(), &[]);
    let control_stderr = String::from_utf8_lossy(&control_out.stderr);
    assert!(
        control_stderr.contains("2 built"),
        "positive control: non-quiet run must print '2 built' so the quiet absence assertion \
         is non-vacuous; got: {control_stderr:?}"
    );
}

// ── AC-Q02: quiet build with a failing file still prints the summary ──────────
//
// When any file fails, the summary is always printed under --quiet so the
// non-zero exit is explained.  Exit codes are unaffected (AD-216-2).

#[test]
fn dir_build_quiet_still_prints_summary_on_failure() {
    let src = tempfile::tempdir().unwrap();
    create_plain_mds(src.path(), "good.mds");
    create_bad_mds(src.path(), "bad.mds");

    let output = build_dir(src.path(), &["--quiet"]);

    assert_eq!(
        output.status.code(),
        Some(1),
        "quiet build with a failing file must exit 1; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("1 built"),
        "AC-Q02: --quiet build with failures must print summary so non-zero exit is explained; \
         got: {stderr:?}"
    );
    assert!(
        stderr.contains("1 failed"),
        "AC-Q02: summary must include '1 failed'; got: {stderr:?}"
    );
}

// ── AC-Q30 (build half): --quiet in both argument positions (global flag) ─────
//
// --quiet is declared `global = true` (main.rs:31), so `mds --quiet build <dir>`
// and `mds build <dir> --quiet` must behave identically.

#[test]
fn dir_build_quiet_works_in_pre_subcommand_position() {
    let src = tempfile::tempdir().unwrap();
    create_plain_mds(src.path(), "a.mds");
    create_plain_mds(src.path(), "b.mds");

    // --quiet after subcommand (post-subcommand position).
    let post = build_dir(src.path(), &["--quiet"]);

    // --quiet before subcommand (pre-subcommand, global position).
    let pre = mds_bin()
        .arg("--quiet")
        .arg("build")
        .arg(src.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert_eq!(
        pre.status.code(),
        post.status.code(),
        "pre-subcommand --quiet must produce the same exit code as post-subcommand --quiet"
    );
    assert_eq!(
        pre.stderr, post.stderr,
        "pre-subcommand --quiet must produce the same stderr as post-subcommand --quiet"
    );

    // ANCHOR (PF-013): equality alone is vacuous — it holds just as well when BOTH
    // orderings are broken. Verified by mutation: with the `!quiet || fail_count > 0`
    // gate removed from build.rs, the two `assert_eq!`s above still passed while
    // `dir_build_quiet_suppresses_summary_on_success` correctly failed. Pin the
    // absolute value so this test fails too if --quiet stops suppressing.
    assert!(
        post.stderr.is_empty(),
        "AC-Q30 anchor: an all-success tree under --quiet must produce empty stderr in \
         BOTH orderings, not merely equal stderr; got: {}",
        String::from_utf8_lossy(&post.stderr)
    );

    // Paired positive control: the same sources without --quiet DO print the summary.
    let control_src = tempfile::tempdir().unwrap();
    create_plain_mds(control_src.path(), "a.mds");
    create_plain_mds(control_src.path(), "b.mds");
    let control = build_dir(control_src.path(), &[]);
    assert!(
        String::from_utf8_lossy(&control.stderr).contains("2 built, 0 failed"),
        "positive control: the same tree without --quiet must print '2 built, 0 failed'; got: {}",
        String::from_utf8_lossy(&control.stderr)
    );
}

// ── R5 (v0.4.0 dogfood): build summary counts zero-byte outputs ──────────────
//
// A definitions-only module (only @define/@export, no body) compiles
// successfully to ZERO bytes, so "N built" silently implies N useful artifacts
// when some are empty (PF-034-adjacent: a definitions-only @include shipping an
// empty section was normalized as noise).  R5: the summary becomes
// `{ok} built ({empty} empty), {fail} failed` — the clause appears ONLY when
// empty > 0 (zero golden churn otherwise), "empty" is STRICT zero bytes (not
// trim — messages-mode JSON output is never empty), and the summary/quiet gate
// (`!quiet || fail_count > 0`) is unchanged.

fn create_defs_only_mds(dir: &Path, name: &str) {
    // Compiles successfully to zero bytes: @define/@export contribute nothing
    // to the compiled body.
    fs::write(
        dir.join(name),
        "@define greet(name):\n  Hi {{name}}\n@end\n\n@export greet\n",
    )
    .unwrap();
}

#[test]
fn d5_dir_build_summary_reports_empty_outputs() {
    let src = tempfile::tempdir().unwrap();
    create_plain_mds(src.path(), "a.mds");
    create_defs_only_mds(src.path(), "lib.mds");

    let output = build_dir(src.path(), &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "both files compile successfully; stderr: {stderr}"
    );
    // Non-vacuity: the zero-byte artifact must actually exist and be empty.
    let lib_out = src.path().join("lib.md");
    assert_eq!(
        fs::metadata(&lib_out).map(|m| m.len()).ok(),
        Some(0),
        "fixture invariant: lib.md must be a zero-byte output"
    );
    assert!(
        stderr.contains("2 built (1 empty), 0 failed"),
        "R5: summary must count zero-byte outputs; got: {stderr}"
    );
}

#[test]
fn d5_dir_build_summary_omits_empty_clause_when_none() {
    let src = tempfile::tempdir().unwrap();
    create_plain_mds(src.path(), "a.mds");

    let output = build_dir(src.path(), &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("1 built, 0 failed"),
        "summary keeps its historical form when no output is empty; got: {stderr}"
    );
    assert!(
        !stderr.contains("empty"),
        "R5: the empty clause must be ABSENT when every output has content \
         (no golden churn); got: {stderr}"
    );
}

#[test]
fn d5_dir_build_quiet_gate_unchanged_with_empty_outputs() {
    // The summary gate stays `!quiet || fail_count > 0`: empty outputs do NOT
    // force the summary through --quiet on an all-success run.
    let src = tempfile::tempdir().unwrap();
    create_plain_mds(src.path(), "a.mds");
    create_defs_only_mds(src.path(), "lib.mds");

    let output = build_dir(src.path(), &["--quiet"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "all-success run; stderr: {stderr}");
    assert!(
        !stderr.contains("built"),
        "R5: --quiet must still suppress the summary on an all-success run even \
         when empty outputs exist (gate unchanged); got: {stderr}"
    );
}

// ── Atomic directory-mode outputs (#227) ─────────────────────────────────────
//
// Directory mode has its own writer (it accumulates per-file counters instead of
// returning early), so it is a second site with the same obligation as `write_output`:
// every compiled artifact and every `.map` sidecar goes through
// `crate::output::atomic_write_file`.

/// Every `.mds-tmp-` prefixed entry anywhere under `dir` (the temp-file prefix used by
/// `atomic_write_file`). Empty means no write left residue.
fn temp_residue_recursive(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    // Bounded: the output tree is finite and acyclic (read_dir does not follow symlinks).
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".mds-tmp-") {
                found.push(p.display().to_string());
            }
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(p);
            }
        }
    }
    found
}

/// T-D1: a clean `--out-dir --source-map` run writes every artifact and every sidecar
/// and leaves no temp file anywhere in the output tree.
#[test]
fn dir_build_out_dir_source_map_no_temp_residue() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    for name in ["one.mds", "two.mds", "three.mds"] {
        create_plain_mds(src.path(), name);
    }

    let output = build_dir(
        src.path(),
        &["--out-dir", out.path().to_str().unwrap(), "--source-map"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "dir build with --source-map must succeed; stderr: {stderr}"
    );

    for stem in ["one", "two", "three"] {
        let md = out.path().join(format!("{stem}.md"));
        let map = out.path().join(format!("{stem}.md.map"));
        assert!(md.is_file(), "{stem}.md must be written");
        assert!(map.is_file(), "{stem}.md.map sidecar must be written");
    }
    let residue = temp_residue_recursive(out.path());
    assert!(
        residue.is_empty(),
        "a successful dir build must leave no temp file; found: {residue:?}"
    );
}

/// T-D2: when the output directory is not writable, every pre-existing artifact survives
/// intact, the run reports the failures, and nothing is left behind.
#[cfg(unix)]
#[test]
fn dir_build_write_failure_preserves_existing_outputs() {
    use std::os::unix::fs::PermissionsExt as _;

    let src = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("out");
    fs::create_dir(&out).unwrap();

    create_plain_mds(src.path(), "a.mds");
    create_plain_mds(src.path(), "b.mds");
    fs::write(out.join("a.md"), "OLD").unwrap();
    fs::write(out.join("b.md"), "OLD").unwrap();

    fs::set_permissions(&out, fs::Permissions::from_mode(0o555)).unwrap();
    let output = build_dir(src.path(), &["--out-dir", out.to_str().unwrap()]);
    // Restore writability BEFORE asserting so a failing assertion cannot leave an
    // undeletable tempdir behind.
    let _ = fs::set_permissions(&out, fs::Permissions::from_mode(0o755));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_ne!(
        output.status.code(),
        Some(0),
        "a dir build into a read-only output dir must fail; stderr: {stderr}"
    );
    assert!(
        stderr.contains("0 built"),
        "the summary must report nothing built; got: {stderr}"
    );
    assert!(
        stderr.contains("failed"),
        "the summary must report the failures; got: {stderr}"
    );
    assert!(
        stderr.contains("error:"),
        "each failure must be reported on stderr; got: {stderr}"
    );
    assert!(
        stderr.contains("a.md"),
        "the per-file error must name the artifact; got: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(out.join("a.md")).unwrap(),
        "OLD",
        "a.md must survive the failed write"
    );
    assert_eq!(
        fs::read_to_string(out.join("b.md")).unwrap(),
        "OLD",
        "b.md must survive the failed write"
    );
    let residue = temp_residue_recursive(&out);
    assert!(
        residue.is_empty(),
        "a failed dir build must leave no temp file; found: {residue:?}"
    );
}

/// T-D3: the directory-mode `.map` sidecar writer is its own site — a symlinked sidecar
/// path is refused, the artifact beside it is still written, and the run reports one
/// failure.
#[cfg(unix)]
#[test]
fn dir_build_source_map_sidecar_symlink_target_rejected() {
    let src = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("out");
    fs::create_dir(&out).unwrap();

    create_plain_mds(src.path(), "page.mds");
    let real = root.path().join("real.map");
    fs::write(&real, "OLD").unwrap();
    std::os::unix::fs::symlink(&real, out.join("page.md.map")).unwrap();

    let output = build_dir(
        src.path(),
        &["--out-dir", out.to_str().unwrap(), "--source-map"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_ne!(
        output.status.code(),
        Some(0),
        "a symlinked sidecar must fail the dir build; stderr: {stderr}"
    );
    assert!(
        out.join("page.md").is_file(),
        "the compiled artifact is written before the sidecar and must survive"
    );
    assert!(
        stderr.contains("symlink"),
        "the refusal must say why; got: {stderr}"
    );
    assert!(
        stderr.contains("1 failed"),
        "the summary must report exactly one failure; got: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(&real).unwrap(),
        "OLD",
        "the symlink target must not be written through"
    );
}

// ── #217: output-path invariants (flatten visibility, non-UTF-8 paths) ────────

/// #217: `mds build` on a path that cannot be named in UTF-8 exits 2 and writes
/// nothing — no compiled output, and no `.map` sidecar.
///
/// Positive control (a PIN on existing behaviour): the same invocation on a normally
/// named file exits 0 and writes a sidecar whose `sources` names the source. Without
/// it, "no `.map` was written" in the hostile arm would be indistinguishable from a
/// build that never emits sidecars at all.
///
/// The invalid bytes are built at RUNTIME from numeric values; no escape sequence or
/// raw byte appears in this source file (source hygiene gate).
#[cfg(unix)]
#[test]
fn build_non_utf8_path_exits_2_and_writes_no_map() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    // CONTROL ARM — a normally named source, same flags.
    let control = tempfile::tempdir().unwrap();
    create_plain_mds(control.path(), "ok.mds");
    let control_out = build_file(&control.path().join("ok.mds"), &["--source-map"]);
    let control_stderr = String::from_utf8_lossy(&control_out.stderr);
    assert_eq!(
        control_out.status.code(),
        Some(0),
        "control: a normally named source must build; stderr: {control_stderr}"
    );
    let map_path = control.path().join("ok.md.map");
    assert!(
        map_path.is_file(),
        "control: --source-map must write a sidecar, or the hostile arm's \
         'no .map' assertion proves nothing"
    );
    let map: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&map_path).unwrap()).expect("sidecar is JSON");
    let sources: Vec<&str> = map["sources"]
        .as_array()
        .expect("sources must be an array")
        .iter()
        .filter_map(|s| s.as_str())
        .collect();
    assert_eq!(
        sources,
        vec!["ok.mds"],
        "control: the sidecar must name the source it was built from"
    );

    // HOSTILE ARM — a path that is not valid UTF-8.
    // 0xFF and 0xFE are not legal UTF-8 lead bytes in any position. The `.mds`
    // extension is itself valid UTF-8, so the extension gate still accepts the entry.
    let dir = tempfile::tempdir().unwrap();
    let raw: Vec<u8> = vec![0xff, 0xfe, b'.', b'm', b'd', b's'];
    let hostile = dir.path().join(OsString::from_vec(raw));

    if fs::write(&hostile, "Hello, world!\n").is_err() {
        // macOS (APFS / HFS+) enforces valid UTF-8 in filenames and rejects this create
        // with EILSEQ, so the ON-DISK half is a Linux-CI gate. The control arm above has
        // already run here. Any OTHER unix filesystem must accept the name and reach the
        // assertions below — panic rather than skip silently, so a genuine regression
        // can never masquerade as a skip.
        #[cfg(not(target_os = "macos"))]
        panic!(
            "build_non_utf8_path_exits_2_and_writes_no_map: a non-UTF-8 filename was \
             rejected by the filesystem — unexpected on this platform"
        );
        #[cfg(target_os = "macos")]
        return;
    }

    let output = build_file(&hostile, &["--source-map"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "#217: a path that cannot be named must be an I/O failure; stderr: {stderr}"
    );
    assert!(
        stderr.contains("not valid UTF-8"),
        "the diagnostic must say why; got: {stderr}"
    );

    let names: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names.len(),
        1,
        "neither an output nor a sidecar may be written for a rejected path; got {names:?}"
    );
}

/// #217 control: a root reached through a symlinked ANCESTOR still mirrors its subtree,
/// and the out-of-root flatten is NOT reported.
///
/// A symlinked root itself is rejected (`dir_build_symlinked_entry_root_rejected`), so
/// the non-canonical shape has to come from an ancestor. `run_build_directory` hands the
/// same raw `dir` value to the walker and to `output_path_for`, so `strip_prefix`
/// succeeds and the mirror survives. This test is what fails if a future change
/// canonicalizes one of the two and not the other.
#[cfg(unix)]
#[test]
fn dir_build_symlinked_ancestor_root_mirrors_without_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let real_src = tmp.path().join("real").join("src");
    fs::create_dir_all(real_src.join("sub")).unwrap();
    create_plain_mds(&real_src, "a.mds");
    create_plain_mds(&real_src.join("sub"), "b.mds");
    std::os::unix::fs::symlink(tmp.path().join("real"), tmp.path().join("link")).unwrap();

    let out = tempfile::tempdir().unwrap();
    let output = build_dir(
        &tmp.path().join("link").join("src"),
        &["--out-dir", out.path().to_str().unwrap()],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(0),
        "a root under a symlinked ancestor must build; stderr: {stderr}"
    );
    assert!(
        out.path().join("a.md").is_file(),
        "top-level source must mirror into the out-dir; stderr: {stderr}"
    );
    assert!(
        out.path().join("sub").join("b.md").is_file(),
        "the subtree must be preserved, not flattened; stderr: {stderr}"
    );
    assert!(
        stderr.contains("2 built, 0 failed"),
        "control needle: the real summary must be present, or the negative assertion \
         below would pass on empty stderr; got: {stderr}"
    );
    assert!(
        !stderr.contains("is outside the build root"),
        "a mirrored build must not report the out-of-root flatten; got: {stderr}"
    );
}

/// #217 control: a root containing a `..` component mirrors its subtree and does not
/// report the out-of-root flatten. Same property as the symlinked-ancestor test, via
/// the other way a caller can hand in a non-canonical root.
#[test]
fn dir_build_dotdot_root_mirrors_without_warning() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("sub")).unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(src.join("nested")).unwrap();
    create_plain_mds(&src, "a.mds");
    create_plain_mds(&src.join("nested"), "b.mds");

    let out = tempfile::tempdir().unwrap();
    let output = build_dir(
        &tmp.path().join("sub").join("..").join("src"),
        &["--out-dir", out.path().to_str().unwrap()],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(0),
        "a root with a '..' component must build; stderr: {stderr}"
    );
    assert!(
        out.path().join("a.md").is_file(),
        "top-level source must mirror into the out-dir; stderr: {stderr}"
    );
    assert!(
        out.path().join("nested").join("b.md").is_file(),
        "the subtree must be preserved, not flattened; stderr: {stderr}"
    );
    assert!(
        stderr.contains("2 built, 0 failed"),
        "control needle: the real summary must be present, or the negative assertion \
         below would pass on empty stderr; got: {stderr}"
    );
    assert!(
        !stderr.contains("is outside the build root"),
        "a mirrored build must not report the out-of-root flatten; got: {stderr}"
    );
}
