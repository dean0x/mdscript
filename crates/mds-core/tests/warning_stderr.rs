//! #435: warning printing must tolerate a closed stderr pipe.
//!
//! A child process runs the public virtual-compile entry point after moving its
//! stderr onto a pipe whose read end is closed before spawn. The unchanged
//! `eprintln!` implementation exits 101; a checked write returns the compile
//! result normally. The command deliberately swallows advisory output failures
//! while preserving successful compilation and the collected-warning APIs.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};

const CHILD_TEST: &str = "writes_warning_through_public_compile";

fn child_command() -> Command {
    let exe = std::env::current_exe().expect("integration test executable");
    let mut command = Command::new(exe);
    command
        .arg("--exact")
        .arg(CHILD_TEST)
        .arg("--ignored")
        .arg("--nocapture")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    command
}

#[test]
fn warnings_do_not_panic_when_stderr_is_closed() {
    let (reader, writer) = std::io::pipe().expect("create stderr pipe");
    drop(reader);

    let mut child = child_command()
        .stderr(Stdio::from(writer))
        .spawn()
        .expect("spawn child");
    let status = child.wait().expect("wait for child");

    assert!(
        status.success(),
        "child must finish the compile without panicking when stderr is closed: {status}"
    );
}

#[test]
fn open_stderr_receives_the_warning_and_compile_succeeds() {
    let mut child = child_command()
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn child");
    let mut output = String::new();
    child
        .stderr
        .as_mut()
        .expect("piped stderr")
        .read_to_string(&mut output)
        .expect("read child stderr");
    let status = child.wait().expect("wait for child");

    assert!(
        status.success(),
        "the compile must succeed with stderr open: {status}"
    );
    assert_eq!(
        output,
        "warning: @include of 'e' produced empty output — module has no body text; \
         to use the module's functions, call them directly: {{e.fn()}}\n"
    );
}

#[test]
#[ignore]
fn writes_warning_through_public_compile() {
    let modules = HashMap::from([
        (
            "main.mds".to_owned(),
            "---\nimports:\n  - path: ./empty.mds\n    as: e\n---\n@include e\n".to_owned(),
        ),
        ("empty.mds".to_owned(), String::new()),
    ]);
    let result = mds::compile_virtual(modules, "main.mds", None)
        .expect("compile must succeed")
        .into_markdown()
        .expect("markdown output");
    assert_eq!(result, "");
}
