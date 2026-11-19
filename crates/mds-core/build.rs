use std::{env, fs, io, path::PathBuf};

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=build.rs");
    let out = env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("Cargo did not set OUT_DIR"))?;

    // Use the toolchain's Unicode data. Rust does not expose the Cased and
    // Case_Ignorable properties used by str::to_lowercase on stable Rust.
    let mut transitions = Vec::new();
    let mut previous = None;
    let mut probe = String::with_capacity(8);
    for c in (0..=0x10ffff).filter_map(char::from_u32) {
        probe.clear();
        probe.push_str("AΣ");
        probe.push(c);
        // With no final letter, only a cased neighbor keeps the medial sigma.
        let class = if probe.to_lowercase().starts_with("aσ") {
            1 // Cased.
        } else {
            probe.push('A');
            if probe.to_lowercase().starts_with("aσ") {
                2 // Ignorable. Ignore it before the test for Cased.
            } else {
                0 // Uncased.
            }
        };
        if Some(class) != previous {
            // A transition packs 21 code point bits and two class bits.
            let entry = ((c as u32) << 2) | class;
            transitions.extend_from_slice(&entry.to_le_bytes()[..3]);
            previous = Some(class);
        }
    }
    fs::write(out.join("case_context.bin"), transitions)
}
