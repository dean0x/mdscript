//! Build script for the PyO3 extension.
//!
//! With the `extension-module` feature, PyO3 deliberately does not link libpython —
//! the `Py*` symbols are resolved by the interpreter that loads the module. On Linux
//! a `cdylib` may leave those symbols undefined and still link. macOS is stricter:
//! the linker rejects undefined symbols in a `cdylib` unless it is told to defer
//! them, so a bare `cargo build`/`cargo test` on macOS fails with "symbol(s) not
//! found" even though the compile is clean. `maturin` passes the deferral flags when
//! it builds the wheel; this emits the same flag so plain `cargo` builds link too.
//!
//! `rustc-cdylib-link-arg` is scoped to THIS crate's `cdylib` target only, so it does
//! not touch the `mds-cli` executable or any test-harness binaries in the workspace.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-cdylib-link-arg=-undefined");
        println!("cargo:rustc-cdylib-link-arg=dynamic_lookup");
    }
}
