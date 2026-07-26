mod common;
use common::{assert_no_control_chars, fixture, mds_bin};
use std::collections::HashMap;

#[test]
fn file_size_limit_rejects_huge_file() {
    let dir = tempfile::tempdir().unwrap();
    let huge = dir.path().join("huge.mds");
    // Create a file just over 10MB
    let content = "x".repeat(10 * 1024 * 1024 + 1);
    std::fs::write(&huge, &content).unwrap();
    let result = mds::compile(&huge, None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("file too large"),
        "Expected 'file too large' error, got: {err}"
    );
}

#[test]
fn path_traversal_import_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();

    // Create a file outside 'sub' directory
    let outside = dir.path().join("secret.mds");
    std::fs::write(&outside, "Secret content").unwrap();

    // Create a file inside 'sub' that tries to import outside its root
    let child = sub.join("child.mds");
    std::fs::write(&child, "@import \"../secret.mds\" as s\n\n@include s\n").unwrap();

    let result = mds::compile(&child, None);
    assert!(result.is_err(), "Traversal import should be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("escapes project directory") || err.contains("import"),
        "Expected path traversal error, got: {err}"
    );
}

#[test]
fn import_depth_limit() {
    let dir = tempfile::tempdir().unwrap();
    // Create a chain of 66 files, each importing the next
    let depth = 66;
    for i in 0..depth {
        let name = format!("mod_{i}.mds");
        let content = if i < depth - 1 {
            let next = format!("mod_{}.mds", i + 1);
            format!("@import \"./{next}\" as m{}\n\nLevel {i}\n", i + 1)
        } else {
            format!("End of chain {i}\n")
        };
        std::fs::write(dir.path().join(&name), content).unwrap();
    }
    let result = mds::compile(dir.path().join("mod_0.mds"), None);
    assert!(result.is_err(), "Deep import chain should be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("import depth") || err.contains("64"),
        "Expected import depth error, got: {err}"
    );
}

#[test]
fn stdin_size_limit_rejects_oversized_input() {
    // Feed more than 10 MB to stdin and verify the CLI returns a non-zero exit code
    // with an appropriate error message.
    use std::io::Write;

    let oversized: Vec<u8> = vec![b'x'; 10 * 1024 * 1024 + 1];

    let mut child = mds_bin()
        .args(["build", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Write oversized data; ignore broken-pipe errors (process may exit early).
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&oversized);
    }

    let output = child.wait_with_output().unwrap();
    assert!(
        !output.status.success(),
        "build from oversized stdin must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("exceeds maximum") || stderr.contains("10 MB"),
        "error should mention size limit, got: {stderr}"
    );
}

#[test]
fn vars_file_size_limit_rejects_oversized_file() {
    let dir = tempfile::tempdir().unwrap();
    let huge_vars = dir.path().join("huge_vars.json");

    // Create a JSON file just over 10 MB (fill a JSON object with long values)
    let mut content = String::from("{\"key\": \"");
    content.push_str(&"x".repeat(10 * 1024 * 1024));
    content.push_str("\"}");
    std::fs::write(&huge_vars, &content).unwrap();

    let result = mds::load_vars_file(&huge_vars);
    assert!(result.is_err(), "oversized vars file must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("exceeds maximum size") || err.contains("vars file"),
        "error should mention vars file size limit, got: {err}"
    );
}

#[test]
fn for_loop_iteration_limit_rejects_huge_array() {
    // Build a source that iterates over an array larger than MAX_LOOP_ITERATIONS (100_000).
    // We construct the array via runtime vars to avoid a huge source string.
    let huge_array: Vec<mds::Value> = (0..100_001)
        .map(|i| mds::Value::String(format!("item{i}")))
        .collect();
    let mut vars = HashMap::new();
    vars.insert("items".to_string(), mds::Value::Array(huge_array));

    let result = mds::compile(fixture("loop.mds"), Some(vars));
    assert!(result.is_err(), "loop over 100_001 items must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("exceeding maximum loop iteration limit")
            || err.contains("100001")
            || err.contains("100_001"),
        "error should mention iteration limit, got: {err}"
    );
}

#[test]
#[cfg(unix)]
fn symlink_import_rejected() {
    let dir = tempfile::tempdir().unwrap();

    // Create a real .mds file to be the symlink target
    let real_file = dir.path().join("real.mds");
    std::fs::write(&real_file, "@define greet(name):\nHello {{name}}!\n@end\n").unwrap();

    // Create a symlink pointing to it
    let link_file = dir.path().join("linked.mds");
    std::os::unix::fs::symlink(&real_file, &link_file).unwrap();

    // Create a consumer that imports via the symlink
    let consumer = dir.path().join("consumer.mds");
    std::fs::write(
        &consumer,
        "@import { greet } from \"./linked.mds\"\n{{greet(\"Alice\")}}\n",
    )
    .unwrap();

    let result = mds::compile(&consumer, None);
    assert!(result.is_err(), "import via symlink must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("symlink") || err.contains("not allowed"),
        "error should mention symlink restriction, got: {err}"
    );
}

#[test]
fn nested_loop_total_iteration_limit() {
    // Two nested loops of 1001 × 1000 = 1,001,000 total iterations must be rejected.
    // Build the source inline: outer array has 1001 elements, inner has 1000.
    let outer: Vec<String> = (0..1001).map(|i| format!("o{i}")).collect();
    let inner: Vec<String> = (0..1000).map(|i| format!("i{i}")).collect();
    let outer_yaml = outer.join(", ");
    let inner_yaml = inner.join(", ");
    let source = format!(
        "---\nouter: [{outer_yaml}]\ninner: [{inner_yaml}]\n---\n@for x in outer:\n@for y in inner:\n{{x}}-{{y}}\n@end\n@end\n"
    );
    let result = mds::compile_str_with(&source, None, None);
    assert!(
        result.is_err(),
        "nested loops exceeding 1M total iterations must be rejected"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("total loop iterations") || err.contains("1000000"),
        "error should mention total iteration limit, got: {err}"
    );
}

#[test]
fn nested_loop_under_total_iteration_limit() {
    // With unconditional iteration counting, every iteration across all loops counts.
    // 999 outer × 1000 inner = 999,000 inner iterations + 999 outer = 999,999 total.
    // This stays under MAX_TOTAL_ITERATIONS (1,000,000) and must succeed.
    let outer: Vec<String> = (0..999).map(|i| format!("o{i}")).collect();
    let inner: Vec<String> = (0..1000).map(|i| format!("i{i}")).collect();
    let outer_yaml = outer.join(", ");
    let inner_yaml = inner.join(", ");
    let source = format!(
        "---\nouter: [{outer_yaml}]\ninner: [{inner_yaml}]\n---\n@for x in outer:\n@for y in inner:\nx\n@end\n@end\n"
    );
    let result = mds::compile_str_with(&source, None, None);
    assert!(
        result.is_ok(),
        "nested loops under 1M total iterations must succeed, got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn resolve_source_nonexistent_base_dir_errors() {
    // Passing a non-existent base_dir to compile_str_with must now return an error
    // (previously silently fell back to the raw path).
    let nonexistent_dir = std::path::Path::new("/nonexistent/path/that/does/not/exist");
    let result = mds::compile_str_with(
        "---\nname: World\n---\nHello {{name}}!\n",
        Some(nonexistent_dir),
        None,
    );
    assert!(
        result.is_err(),
        "non-existent base_dir must now return an error"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("cannot resolve base directory") || err.contains("nonexistent"),
        "error should describe the unresolvable base directory, got: {err}"
    );
}

#[test]
fn resolve_source_import_traversal_rejected() {
    // Security invariant (AC-S2): the string + base_dir route must reject a
    // relative `@import` that escapes `base_dir` via `../`, exactly like the
    // file route (`path_traversal_import_rejected` above). Regression guard for
    // PF-003 / #133 / #146 directory-anchored resolution — `ctx.base_dir` still
    // routes through the same path-traversal check (`check_path_traversal`).
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();

    // Create a file outside the 'sub' base_dir.
    let outside = dir.path().join("secret.mds");
    std::fs::write(&outside, "Secret content").unwrap();

    let source = "@import \"../secret.mds\" as s\n\n@include s\n";
    let result = mds::compile_str_with(source, Some(sub.as_path()), None);
    assert!(
        result.is_err(),
        "string-route traversal import should be rejected"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("escapes project directory") || err.contains("import"),
        "Expected path traversal error, got: {err}"
    );
}

#[test]
fn parser_nesting_depth_limit_rejects_deep_nesting() {
    // Build a template with 65 nested @if blocks (just past MAX_NESTING_DEPTH=64).
    //
    // 65 levels is well within the default 2 MB thread stack, so no enlarged
    // stack is required. The depth limit error must fire before any stack overflow.
    let mut source = String::new();
    source.push_str("---\nflag: true\n---\n");
    for _ in 0..65 {
        source.push_str("@if flag:\n");
    }
    source.push_str("deep\n");
    for _ in 0..65 {
        source.push_str("@end\n");
    }

    let result = mds::compile_str(&source);
    assert!(result.is_err(), "65 nested @if blocks must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("nesting") || err.contains("depth") || err.contains("64"),
        "error should mention nesting depth limit, got: {err}"
    );
}

#[test]
fn build_mds_json_output_dir_path_traversal_rejected() {
    // mds.json with `output_dir` containing `..` components must be rejected
    // by resolve_output_path to prevent writing files outside the project root.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "Hello!\n").unwrap();
    std::fs::write(
        dir.path().join("mds.json"),
        r#"{"build": {"output_dir": "../escaped"}}"#,
    )
    .unwrap();

    let output = mds_bin()
        .arg("build")
        .arg(&src)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "build with output_dir containing '..' must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("..") || stderr.contains("output_dir"),
        "error should mention the traversal or output_dir, got: {stderr}"
    );
}

#[test]
fn config_size_limit_rejects_oversized_mds_json() {
    // mds.json files larger than MAX_CONFIG_SIZE (1 MB) must be rejected to
    // prevent runaway memory use when walking up the directory tree.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "Hello!\n").unwrap();

    // Build a JSON file just over 1 MB by padding the output_dir value.
    let padding = "x".repeat(1024 * 1024);
    let huge_config = format!(r#"{{"build": {{"output_dir": "{padding}"}}}}"#);
    std::fs::write(dir.path().join("mds.json"), &huge_config).unwrap();

    let output = mds_bin()
        .arg("build")
        .arg(&src)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "build with oversized mds.json must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("too large") || stderr.contains("1 MB") || stderr.contains("mds.json"),
        "error should mention config size limit, got: {stderr}"
    );
}

#[test]
fn exit_code_resource_limit() {
    // Trigger MAX_TOTAL_ITERATIONS (1,000,000) via two nested @for loops.
    // Outer array: 1,001 items × inner array: 1,001 items = 1,002,001 iterations.
    // Each individual loop stays well under MAX_LOOP_ITERATIONS (100,000), so
    // only MAX_TOTAL_ITERATIONS fires, producing MdsError::ResourceLimit → exit code 3.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("resource_limit.mds");

    // Build a YAML frontmatter array of 1,001 string items and a nested @for loop.
    let mut source = String::from("---\n");
    source.push_str("outer:\n");
    for i in 0..1001 {
        source.push_str(&format!("  - item{i}\n"));
    }
    source.push_str("inner:\n");
    for i in 0..1001 {
        source.push_str(&format!("  - sub{i}\n"));
    }
    source.push_str("---\n");
    source.push_str("@for x in outer:\n");
    source.push_str("@for y in inner:\n");
    source.push_str("{{x}}-{{y}}\n");
    source.push_str("@end\n");
    source.push_str("@end\n");

    std::fs::write(&src, &source).unwrap();

    let status = mds_bin()
        .args(["build"])
        .arg(&src)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("failed to run mds");

    assert_eq!(
        status.code(),
        Some(3),
        "expected exit code 3 for resource-limit error"
    );
}

// ── AC-2: load_vars_file rejects symlinked vars paths (PF-004 fix) ───────────

#[test]
#[cfg(unix)]
fn load_vars_file_rejects_symlinked_path() {
    // Proves the PF-004 fix: load_vars_file now routes the path through
    // NativeFs::check_symlink before reading, the same guard applied to every
    // other file read in the resolver.
    let dir = tempfile::tempdir().unwrap();

    let real_vars = dir.path().join("real_vars.json");
    std::fs::write(&real_vars, r#"{"name": "Alice"}"#).unwrap();

    let link_vars = dir.path().join("link_vars.json");
    std::os::unix::fs::symlink(&real_vars, &link_vars).unwrap();

    let result = mds::load_vars_file(&link_vars);
    assert!(
        result.is_err(),
        "load_vars_file must reject a symlinked vars path"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("symlink") || err.contains("not allowed"),
        "error must mention symlink restriction; got: {err}"
    );
}

// ── CWE-150: control bytes in the error MESSAGE on the CLI human path (#176) ──
//
// The pre-existing ESC e2e tests in `cli_build.rs` / `cli_fmt.rs` / `cli_lint.rs`
// all use the `@define \x1bfoo:` vector, whose message is the *fixed* string
// "syntax error: @define must have parameter list: @define name(params):" — the
// hostile byte only ever reaches the rendered **source excerpt**, which
// `MdsError::at()` already neutralized. Those tests therefore never exercised the
// message text at all (PF-013: a hostile-input test that cannot fail for the
// reason it claims). The two tests below use vectors that put attacker-controlled
// bytes into the message itself, on both of the CLI's error families:
//
//   1. `MdsError` — `parser.rs` formats the raw alias into
//      `invalid include alias: '{alias}'`.
//   2. CLI-authored `miette::miette!()` — `output.rs` formats the raw `mds.json`
//      value into `mds.json output_dir '{}' must not contain '..' components`.
//
// Both render through `eprint_error`, the single CLI stderr choke-point.

/// T-ESC-MSG-1 [security-11 / CWE-150 / PF-013 / #176]: an `MdsError` whose message
/// interpolates attacker-controlled template text must reach stderr escaped.
///
/// Non-vacuity guard: the command must actually fail *and* stderr must contain the
/// expected error text, so the escape assertions cannot pass by the build silently
/// succeeding or failing for an unrelated reason.
#[test]
fn build_mds_error_message_escapes_control_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("inc.mds");
    // ESC (U+001B) and RIGHT-TO-LEFT OVERRIDE (U+202E) inside the include alias.
    // `is_valid_identifier` rejects it, and the raw alias is formatted into the message.
    std::fs::write(&src, "@include \u{1b}[31mBAD\u{202e}alias\n").unwrap();

    let out = mds_bin()
        .arg("build")
        .arg(&src)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    // ── Non-vacuity ──────────────────────────────────────────────────────────
    assert!(
        !out.status.success(),
        "build of an invalid include alias must fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("invalid include alias"),
        "non-vacuity: the alias error must be the one rendered; got: {stderr}"
    );

    // ── Negative: no raw hostile bytes survive ───────────────────────────────
    assert!(
        !out.stderr.contains(&0x1Bu8),
        "raw ESC byte (0x1B) must not reach stderr in the error message; got: {stderr}"
    );
    assert_no_control_chars(&stderr, "mds build MdsError message");

    // ── Positive: the escaped literals are present ───────────────────────────
    assert!(
        stderr.contains("\\u001B"),
        "ESC must be rendered as the \\u001B literal in the message; got: {stderr}"
    );
    assert!(
        stderr.contains("\\u202E"),
        "U+202E must be rendered as the \\u202E literal in the message; got: {stderr}"
    );
}

/// T-ESC-MSG-2 [security-11 / CWE-150 / PF-004 / #176]: a CLI-authored
/// `miette::miette!()` error that interpolates attacker-controlled config text must
/// reach stderr escaped too.
///
/// This is the PF-004 sibling path: `miette!()` reports do **not** downcast to
/// `MdsError` (see `exit_code`'s rustdoc), so a fix that only handled `MdsError`
/// would leave this vector open while claiming the boundary was closed.
#[test]
fn build_cli_authored_error_message_escapes_control_bytes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.mds"), "Hello!\n").unwrap();
    // JSON `\u001b` decodes to a raw ESC byte in the parsed config value. The `..`
    // component trips the traversal guard in `resolve_output_base`, which formats the
    // raw value into its message.
    std::fs::write(
        dir.path().join("mds.json"),
        r#"{"build": {"output_dir": "../\u001b[31mBAD"}}"#,
    )
    .unwrap();

    let out = mds_bin()
        .arg("build")
        .arg(dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    // ── Non-vacuity ──────────────────────────────────────────────────────────
    assert!(
        !out.status.success(),
        "build with output_dir containing '..' must fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("output_dir"),
        "non-vacuity: the output_dir traversal error must be the one rendered; got: {stderr}"
    );

    // ── Negative + positive ──────────────────────────────────────────────────
    assert!(
        !out.stderr.contains(&0x1Bu8),
        "raw ESC byte (0x1B) must not reach stderr from a miette!() message; got: {stderr}"
    );
    assert_no_control_chars(&stderr, "mds build miette!() message");
    assert!(
        stderr.contains("\\u001B"),
        "ESC must be rendered as the \\u001B literal; got: {stderr}"
    );
}

// ── S14 / PF-004: the two boundaries the #176 alignment review found still open ──
//
// Both are reproduced-first vectors: on the pre-fix binary each command below emits
// the raw hostile bytes (verified with `od -c`).

/// T-ESC-RULE-1 [security-11 / CWE-150 / PF-004 / PF-013 / #176]: an unknown lint rule
/// NAME from `mds.json` reaches stderr escaped.
///
/// Vector: `mds.json` is read from the working tree and its rule names are arbitrary
/// JSON object keys. A JSON `\uXXXX` escape decodes to a real byte, so a repository can
/// carry a rule name containing a raw ESC without the file itself looking hostile.
/// This print site sat on a bare `eprintln!`, bypassing both `eprint_error` and
/// `eprint_warning` — the "sixth warning print" that `eprint_warning`'s own rustdoc
/// claimed was foreclosed.
///
/// `serde_json` writes the ESC as a LOWERCASE-hex JSON escape; the sanitizer emits
/// UPPERCASE. Asserting the uppercase form proves the byte genuinely decoded on the way
/// in and was genuinely escaped on the way out, rather than passing through as literal
/// text.
#[test]
fn lint_unknown_rule_name_escapes_control_bytes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.mds"), "Hello!\n").unwrap();

    // One member of each escape sub-class: a C0 byte, a 3-byte bidi control, and the
    // 2-byte bidi control (U+061C) that the class originally missed.
    let rule_name = format!("{}[31mEVIL{}RULE{}ALM", '\u{1b}', '\u{202e}', '\u{061c}');
    let mut rules = serde_json::Map::new();
    rules.insert(rule_name, serde_json::Value::String("warn".to_string()));
    let config = serde_json::json!({ "lint": { "rules": rules } });
    std::fs::write(
        dir.path().join("mds.json"),
        serde_json::to_string(&config).unwrap(),
    )
    .unwrap();

    let out = mds_bin()
        .arg("lint")
        .arg(dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);

    // ── Non-vacuity: the warning actually fired, naming the rule ─────────────
    assert!(
        stderr.contains("unknown lint rule"),
        "non-vacuity: the unknown-rule warning must be the one rendered; got: {stderr}"
    );
    assert!(
        stderr.contains("EVIL"),
        "non-vacuity: the rule name itself must be printed; got: {stderr}"
    );

    // ── Negative: no raw hostile byte survives ───────────────────────────────
    assert!(
        !out.stderr.contains(&0x1Bu8),
        "raw ESC byte (0x1B) must not reach stderr from an mds.json rule name; got: {stderr}"
    );
    assert_no_control_chars(&stderr, "mds lint unknown-rule warning");

    // ── Positive: the escaped literals are present ───────────────────────────
    for escaped in ["\\u001B", "\\u202E", "\\u061C"] {
        assert!(
            stderr.contains(escaped),
            "{escaped} must appear in the unknown-rule warning; got: {stderr}"
        );
    }
}

/// T-ESC-FNAME-1 [S14 / CWE-117 / PF-013 / #176]: a filename containing newlines
/// cannot forge CLI status lines.
///
/// Vector: POSIX permits a newline inside a filename and the user never types the name
/// — `mds build <dir>` discovers it by directory walk. `safe_path` used HUMAN mode,
/// which preserves newlines by design so that multi-line diagnostic *messages* keep
/// rendering, so a file named `evil.mds<LF>Clean: real.mds<LF>OK: all-fine.mds` emitted
/// two attacker-authored lines byte-identical in form to genuine status output,
/// unframed and unindented.
///
/// Unix-only: Windows filesystems reject a newline in a filename outright.
#[cfg(unix)]
#[test]
fn build_status_line_cannot_be_forged_by_a_newline_in_a_filename() {
    let dir = tempfile::tempdir().unwrap();
    let hostile = "evil.mds\nClean: real.mds\nOK: all-fine.mds";
    std::fs::write(dir.path().join(hostile), "Hello!\n").unwrap();
    let out_dir = dir.path().join("out");

    let out = mds_bin()
        .arg("build")
        .arg(dir.path())
        .arg("--out-dir")
        .arg(&out_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // ── Non-vacuity: the build ran and printed a status line naming the file ─
    assert!(
        out.status.success(),
        "the build must succeed; stdout+stderr: {combined}"
    );
    assert!(
        combined.contains("evil.mds"),
        "non-vacuity: the status line must name the built file; got: {combined}"
    );

    // ── Negative: neither forged line appears on a line of its own ───────────
    for forged in ["Clean: real.mds", "OK: all-fine.mds"] {
        assert!(
            !combined.lines().any(|l| l.trim() == forged),
            "a filename must not be able to forge the standalone status line \
             {forged:?}; got: {combined}"
        );
    }

    // ── Positive: both newlines escaped to their literal form ────────────────
    assert_eq!(
        combined.matches("\\u000A").count(),
        2,
        "both embedded newlines must be escaped; got: {combined}"
    );
}

/// T-ESC-FNAME-2 [S14 / PF-004 / #176]: the `Clean:` status line — which sanitized its
/// filename inline rather than through `safe_path` — is covered by the same guarantee.
///
/// This is the PF-004 sibling of T-ESC-FNAME-1: two status-line printers, one of which
/// open-coded its own escape call and so would have kept HUMAN mode when `safe_path`
/// moved to WIRE. The vector also carries U+061C, the bidi control the escape class
/// originally missed, so this pins both fixes on one line of output.
///
/// Unix-only: Windows filesystems reject a newline in a filename outright.
#[cfg(unix)]
#[test]
fn lint_clean_status_line_cannot_be_forged_by_a_newline_in_a_filename() {
    let dir = tempfile::tempdir().unwrap();
    let hostile = "ok.mds\nClean: real\u{061c}.mds";
    let path = dir.path().join(hostile);
    std::fs::write(&path, "Hello!\n").unwrap();

    let out = mds_bin()
        .arg("lint")
        .arg(&path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);

    // ── Non-vacuity: the Clean: line actually printed, naming the file ───────
    assert!(
        stderr.contains("Clean: ok.mds"),
        "non-vacuity: the Clean: status line must name the linted file; got: {stderr}"
    );

    // ── Negative: no forged standalone line, no raw hostile codepoint ────────
    assert!(
        !stderr.lines().any(|l| l.trim().starts_with("Clean: real")),
        "a filename must not be able to forge a second Clean: line; got: {stderr}"
    );
    assert_no_control_chars(&stderr, "mds lint Clean: status line");

    // ── Positive: newline and U+061C both escaped ───────────────────────────
    assert!(
        stderr.contains("\\u000A"),
        "the embedded newline must be escaped; got: {stderr}"
    );
    assert!(
        stderr.contains("\\u061C"),
        "U+061C must be escaped; got: {stderr}"
    );
}
