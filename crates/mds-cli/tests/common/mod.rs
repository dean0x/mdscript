use std::path::PathBuf;

#[allow(dead_code)]
pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[allow(dead_code)]
pub fn mds_bin() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_mds"))
}
