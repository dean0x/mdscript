//! Peak-heap bounds on reading a module (#428): a file over the 10 MiB per-file cap is
//! refused without ever being held in memory whole.
//!
//! `NativeFs::read` read a whole file and only then checked it against the cap, so a
//! module of any size was allocated first — the refusal text is the same either way,
//! so this measures the resource the fix moves (applies PF-013): peak heap growth,
//! under a counting global allocator. A file's size when it is opened decides a
//! regular file over the cap before a byte is read. A module that is not a regular
//! file — a FIFO fed more than the cap — is refused before it is opened, so none of it
//! is read; a vars file, which may be one (process substitution), reports no size and
//! is read to one byte past the cap and no further. The `fs` unit tests pin, on every
//! OS, the bounded read of a source that runs past its size hint. The FIFOs are fed by
//! a separate `dd` process, so nothing but the read allocates in this one while it is
//! measured.
//!
//! This binary holds exactly ONE `#[test]`: the allocator is process-wide, and a second
//! test running on another thread would add its allocations to the measurement.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use mds::{FileSystem, NativeFs, MAX_FILE_SIZE};

/// The per-file cap in bytes.
const CAP: usize = MAX_FILE_SIZE as usize;

/// Allowance over the cap plus one byte for everything a read holds besides the file's
/// bytes: paths, the display name, the error.
const SLACK: usize = 256 * 1024;

/// Live heap bytes, and the high-water mark since `peak_growth` last reset it.
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// The system allocator, counting live bytes. A `realloc` counts as its net change
/// (`new - old`), not as a fresh allocation plus a free.
struct Counting;

fn grow(bytes: usize) {
    let now = CURRENT.fetch_add(bytes, Ordering::SeqCst) + bytes;
    PEAK.fetch_max(now, Ordering::SeqCst);
}

fn shrink(bytes: usize) {
    CURRENT.fetch_sub(bytes, Ordering::SeqCst);
}

// SAFETY: every method forwards to `System` with the caller's arguments unchanged and
// only adjusts the counters, so `System`'s guarantees hold for every returned pointer.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc`'s contract.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc_zeroed`'s contract.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged; `ptr` was allocated by `System` via this type.
        unsafe { System.dealloc(ptr, layout) };
        shrink(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `realloc`'s contract.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            if new_size >= layout.size() {
                grow(new_size - layout.size());
            } else {
                shrink(layout.size() - new_size);
            }
        }
        new_ptr
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Run `f`, returning its result and the peak heap growth above the live bytes at the
/// moment it started. Fixtures are built before the call, so they are not counted.
fn peak_growth<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let start = CURRENT.load(Ordering::SeqCst);
    PEAK.store(start, Ordering::SeqCst);
    let out = f();
    (out, PEAK.load(Ordering::SeqCst) - start)
}

/// Read `path` as a module through `NativeFs`, as the resolver reads an entry: its
/// text's length, or its error's message.
fn read_module(path: &Path) -> Result<usize, String> {
    let fs = NativeFs::new();
    let shown = path.to_str().expect("a UTF-8 temp path");
    let key = fs.resolve_entry(shown).map_err(|e| e.to_string())?;
    fs.read(&key)
        .map(|text| text.len())
        .map_err(|e| e.to_string())
}

/// The refusal of a module of `size` bytes named `name`.
fn too_large(size: usize, name: &str) -> String {
    format!("resource limit exceeded: file too large ({size} bytes, max {CAP} bytes): {name}")
}

/// A FIFO at `path` that a `dd` process feeds `blocks` × 64 KiB of NUL bytes. Kill it
/// once the reader is done: a reader that never opened the FIFO leaves it blocked, and
/// one that stopped early broke its pipe.
#[cfg(unix)]
fn fed_fifo(path: &Path, blocks: usize) -> std::process::Child {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("mkfifo runs");
    assert!(status.success(), "mkfifo {}", path.display());
    std::process::Command::new("dd")
        .arg("if=/dev/zero")
        .arg(format!("of={}", path.display()))
        .arg("bs=65536")
        .arg(format!("count={blocks}"))
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("dd runs")
}

/// Stop the FIFO writer `child`, whether it finished, broke its pipe or never began.
#[cfg(unix)]
fn stop(mut child: std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn a_module_read_holds_at_most_the_cap_plus_one_byte() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::write(dir.path().join(".mdsroot"), "").expect("the root marker");
    // Regular files: far over the cap (sparse, so the disk holds none of it), one byte
    // over it, and exactly at it (the control that must be read whole).
    let sized = |name: &str, size: usize| {
        let path = dir.path().join(name);
        let file = std::fs::File::create(&path).expect("a fixture file");
        file.set_len(size as u64).expect("sized");
        path
    };
    let far = sized("far.mds", 64 * 1024 * 1024);
    let over = sized("over.mds", CAP + 1);
    let exact = sized("exact.mds", CAP);

    let mut measured: Vec<(&str, Result<usize, String>, usize)> = Vec::new();
    for (case, path) in [
        ("64 MiB file", &far),
        ("cap + 1 file", &over),
        ("exactly the cap", &exact),
    ] {
        let (outcome, peak) = peak_growth(|| read_module(path));
        measured.push((case, outcome, peak));
    }

    // A FIFO reports no size: a module one is refused before it is opened, and only the
    // bound on the read itself holds a vars one.
    #[cfg(unix)]
    {
        // 192 blocks of 64 KiB: the cap plus 2 MiB.
        let blocks = (CAP + 2 * 1024 * 1024) / (64 * 1024);
        let module_fifo = dir.path().join("fifo.mds");
        let writer = fed_fifo(&module_fifo, blocks);
        let (outcome, peak) = peak_growth(|| read_module(&module_fifo));
        stop(writer);
        measured.push(("FIFO fed cap + 2 MiB", outcome, peak));

        let vars_fifo = dir.path().join("vars.json");
        let writer = fed_fifo(&vars_fifo, blocks);
        let (outcome, peak) = peak_growth(|| {
            mds::load_vars_file(&vars_fifo)
                .map(|vars| vars.len())
                .map_err(|e: mds::MdsError| e.to_string())
        });
        stop(writer);
        measured.push(("vars FIFO fed cap + 2 MiB", outcome, peak));
    }

    let report = format!("(case, outcome, peak heap growth in bytes): {measured:#?}");
    eprintln!("{report}");
    let expected: Vec<(&str, Result<usize, String>)> = [
        ("64 MiB file", Err(too_large(64 * 1024 * 1024, "far.mds"))),
        ("cap + 1 file", Err(too_large(CAP + 1, "over.mds"))),
        ("exactly the cap", Ok(CAP)),
        (
            "FIFO fed cap + 2 MiB",
            Err("cannot read fifo.mds: not a regular file".to_string()),
        ),
        (
            "vars FIFO fed cap + 2 MiB",
            Err(format!(
                "resource limit exceeded: vars file exceeds maximum size of {CAP} bytes: {}",
                dir.path().join("vars.json").display()
            )),
        ),
    ]
    .into_iter()
    .filter(|(case, _)| measured.iter().any(|(m, _, _)| m == case))
    .collect();
    for ((case, outcome, peak), (_, want)) in measured.iter().zip(&expected) {
        assert_eq!(outcome, want, "{case}; {report}");
        assert!(
            *peak <= CAP + 1 + SLACK,
            "{case}: peak heap growth exceeds the cap plus one byte ({} B); {report}",
            CAP + 1 + SLACK
        );
    }
    // Positive control: the allocator saw the exact-cap file read whole — a counter
    // that missed allocations would pass the bound above vacuously.
    let (_, _, exact_peak) = measured[2];
    assert!(
        exact_peak >= CAP,
        "exactly the cap: peak heap growth is below the {CAP} B read; {report}"
    );
}
