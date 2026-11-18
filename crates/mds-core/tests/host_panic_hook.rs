//! This binary has exactly one test because the panic hook is process-wide.
//! Run all scenarios in order so no other test can replace the hook.

use std::panic::{catch_unwind, panic_any};
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};

use mds::host_panic::{catching, install};

type Records = Arc<Mutex<Vec<(ThreadId, &'static str)>>>;

fn records_for(records: &Records, id: ThreadId) -> Vec<&'static str> {
    records
        .lock()
        .unwrap()
        .iter()
        .filter(|(thread, _)| *thread == id)
        .map(|(_, kind)| *kind)
        .collect()
}

fn assert_payload<T: std::fmt::Debug>(result: thread::Result<T>, kind: &'static str) {
    assert_eq!(result.unwrap_err().downcast_ref::<&str>(), Some(&kind));
}

fn test_count(source: &str) -> usize {
    source
        .lines()
        .filter(|line| line.trim() == "#[test]")
        .count()
}

#[test]
fn the_hook_is_silent_only_within_this_threads_catch_boundary() {
    let source = include_str!("host_panic_hook.rs");
    assert_eq!(test_count(source), 1, "the hook needs one ordered test");
    let extra_test = format!("{source}\n#[test]\nfn extra() {{}}\n");
    assert_eq!(
        test_count(&extra_test),
        2,
        "the source scan must find an extra test"
    );

    let records: Records = Arc::new(Mutex::new(Vec::with_capacity(16)));
    let recorder = Arc::clone(&records);
    std::panic::set_hook(Box::new(move |info| {
        let kind = info
            .payload()
            .downcast_ref::<&'static str>()
            .copied()
            .unwrap_or("other");
        if let Ok(mut records) = recorder.lock() {
            records.push((thread::current().id(), kind));
        }
    }));
    let this_thread = thread::current().id();
    install();

    assert_eq!(catching(|| 7).unwrap(), 7);
    assert_payload(catching(|| panic_any("inside")), "inside");
    assert_eq!(
        records_for(&records, this_thread),
        Vec::<&str>::new(),
        "an inside panic must not call the previous hook"
    );

    assert_payload(catch_unwind(|| panic_any("outside")), "outside");
    assert_eq!(records_for(&records, this_thread), ["outside"]);

    assert_payload(
        catching(|| {
            assert_eq!(catching(|| 11).unwrap(), 11);
            panic_any("after-normal-nested");
        }),
        "after-normal-nested",
    );
    assert_eq!(
        records_for(&records, this_thread),
        ["outside"],
        "a normal nested return must keep the outer boundary active"
    );

    assert_payload(
        catching(|| {
            assert_payload(catching(|| panic_any("nested")), "nested");
            panic_any("after-unwind-nested");
        }),
        "after-unwind-nested",
    );
    assert_eq!(
        records_for(&records, this_thread),
        ["outside"],
        "a nested unwind must keep the outer boundary active"
    );
    assert_payload(
        catch_unwind(|| panic_any("outside-after-nested")),
        "outside-after-nested",
    );
    assert_eq!(
        records_for(&records, this_thread),
        ["outside", "outside-after-nested"]
    );

    // The main thread stays inside its boundary while the second thread panics outside it.
    assert_payload(
        catching(|| {
            let worker = thread::spawn(|| {
                let id = thread::current().id();
                let result = catch_unwind(|| panic_any("other-thread"));
                (id, result)
            });
            let (other_thread, result) = worker.join().unwrap();
            assert_payload(result, "other-thread");
            assert_eq!(records_for(&records, other_thread), ["other-thread"]);
            panic_any("inside-after-worker");
        }),
        "inside-after-worker",
    );
    assert_eq!(
        records_for(&records, this_thread),
        ["outside", "outside-after-nested"]
    );

    install();
    assert_payload(
        catching(|| panic_any("inside-after-install")),
        "inside-after-install",
    );
    assert_payload(
        catch_unwind(|| panic_any("outside-after-install")),
        "outside-after-install",
    );
    assert_eq!(
        records_for(&records, this_thread),
        ["outside", "outside-after-nested", "outside-after-install"],
        "a second install must forward each outside panic exactly once"
    );
}
