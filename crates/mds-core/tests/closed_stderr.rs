//! A closed stderr does not crash a compile that prints its warnings (#435).
//!
//! The non-collecting entry points (`compile`, `compile_str_with`, `check`,
//! `check_str_with`, `compile_virtual`, `check_virtual`) print each warning to stderr.
//! A write to a closed stderr fails; the print must drop that error, not panic.
//!
//! The parent test runs this binary again as a child, with stderr on a pipe whose
//! read end is closed before the child starts. The child first proves that a write
//! to its stderr fails, so the setup is never vacuous, then compiles a template that
//! makes a warning. A panic in the child fails its test, and the child exits non-zero.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

/// Set in the child's environment only; the child test does nothing without it.
const CHILD_ENV: &str = "MDS_CLOSED_STDERR_CHILD";

/// The child test's name, as libtest matches it with `--exact`.
const CHILD_TEST: &str = "closed_stderr_child";

/// Printed to stdout by the child once the failed write to stderr is proven.
const PROBE_DONE: &str = "child: stderr write failed as set up";

/// Printed to stdout by the child once the compile returned.
const COMPILE_DONE: &str = "child: compile returned";

/// Two modules whose `@include` of a module with no output makes one warning.
fn modules_with_one_warning() -> HashMap<String, String> {
    let mut modules = HashMap::new();
    modules.insert(
        "defs.mds".to_string(),
        "@define greet(x):\nHello {{x}}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import \"./defs.mds\" as defs\n@include defs\n{{defs.greet(\"World\")}}\n".to_string(),
    );
    modules
}

#[test]
fn closed_stderr_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    assert!(
        writeln!(std::io::stderr(), "probe").is_err(),
        "setup: a write to the closed stderr must fail"
    );
    println!("{PROBE_DONE}");

    let result = mds::compile_virtual(modules_with_one_warning(), "main.mds", None)
        .expect("the template compiles");
    assert_eq!(
        result.warnings.len(),
        1,
        "the template makes one warning to print: {:?}",
        result.warnings
    );
    assert_eq!(result.into_markdown().unwrap(), "Hello World!\n");
    println!("{COMPILE_DONE}");
}

#[test]
fn a_compile_that_prints_a_warning_to_a_closed_stderr_succeeds() {
    let (reader, writer) = std::io::pipe().expect("create a pipe");
    drop(reader);

    let exe = std::env::current_exe().expect("locate the test binary");
    let output = Command::new(exe)
        .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(writer)
        .output()
        .expect("run the child test");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the child compile must succeed with a closed stderr; status {:?}, stdout:\n{stdout}",
        output.status
    );
    assert!(
        stdout.contains(PROBE_DONE) && stdout.contains(COMPILE_DONE),
        "the child must pass the probe and return from the compile; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("1 passed"),
        "the child test must run, not be filtered out; stdout:\n{stdout}"
    );
}
