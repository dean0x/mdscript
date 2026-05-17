mod common;
use common::{fixture, mds_bin};
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
    std::fs::write(&real_file, "@define greet(name):\nHello {name}!\n@end\n").unwrap();

    // Create a symlink pointing to it
    let link_file = dir.path().join("linked.mds");
    std::os::unix::fs::symlink(&real_file, &link_file).unwrap();

    // Create a consumer that imports via the symlink
    let consumer = dir.path().join("consumer.mds");
    std::fs::write(
        &consumer,
        "@import { greet } from \"./linked.mds\"\n{greet(\"Alice\")}\n",
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
        "---\nname: World\n---\nHello {name}!\n",
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
fn parser_nesting_depth_limit_rejects_deep_nesting() {
    // Build a template with 257 nested @if blocks (just past MAX_NESTING_DEPTH=256).
    let mut source = String::new();
    source.push_str("---\nflag: true\n---\n");
    for _ in 0..257 {
        source.push_str("@if flag:\n");
    }
    source.push_str("deep\n");
    for _ in 0..257 {
        source.push_str("@end\n");
    }

    let result = mds::compile_str(&source);
    assert!(result.is_err(), "257 nested @if blocks must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("nesting") || err.contains("depth") || err.contains("256"),
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
    source.push_str("{x}-{y}\n");
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
