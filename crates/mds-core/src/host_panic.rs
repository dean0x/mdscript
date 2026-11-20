//! Panic boundaries for native host libraries.
//!
//! The helper silences the previous hook (by default, Rust's own hook) only within
//! a boundary on the current thread. Outside it, the previous hook receives the
//! original panic information. Core compile functions do not call this helper.
//!
//! Each binding library links its own copy of std, so each installs its own hook.
//! A later host `set_hook` call replaces this hook. Installation is not repeated
//! per call, because that would race with hooks that the host installs.
//! With `panic = "abort"`, a panic cannot return through the catch boundary.
//! WASM bindings do not install a hook: wasm32-unknown-unknown has no default hook
//! output channel, and a panic traps.

use std::cell::Cell;
use std::panic::UnwindSafe;
use std::sync::Once;

thread_local! {
    static INSIDE: Cell<bool> = const { Cell::new(false) };
}

static INSTALL: Once = Once::new();

fn inside() -> bool {
    INSIDE.try_with(Cell::get).unwrap_or(false)
}

/// Install the hook once for this library. Call only outside a panic hook.
///
/// The hook does not inspect or format panic information. It calls the previous
/// hook (by default, Rust's own hook) only outside this thread's boundary.
/// If thread-local state is unavailable, it calls the previous hook as well.
/// A later host `set_hook` call replaces this hook; another `install` cannot restore it.
pub fn install() {
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !inside() {
                previous(info);
            }
        }));
    });
}

struct Boundary {
    previous: bool,
}

impl Drop for Boundary {
    fn drop(&mut self) {
        // If TLS is unavailable, there is no state left to restore.
        let _ = INSIDE.try_with(|flag| flag.set(self.previous));
    }
}

/// Run `f` within a silent panic boundary and return its value or panic payload.
///
/// Install the hook before the first boundary. Restore the previous thread-local
/// flag on return or unwind, so nested boundaries keep the outer boundary active.
/// This does not change the panic payload or catch a panic on another thread.
pub fn catching<T>(f: impl FnOnce() -> T + UnwindSafe) -> std::thread::Result<T> {
    install();
    let _boundary = Boundary {
        previous: INSIDE.with(|flag| flag.replace(true)),
    };
    std::panic::catch_unwind(f)
}
