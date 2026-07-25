use std::path::PathBuf;

#[allow(dead_code)]
pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Return a `Command` for the `mds` binary with `NO_COLOR=1` set so that
/// miette does not emit ANSI SGR codes.  Tests that inspect raw stderr/stdout
/// bytes for control-character sanitization must not see miette's own escape
/// sequences, and suppressing colour globally is the safest way to ensure that.
#[allow(dead_code)]
pub fn mds_bin() -> std::process::Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_mds"));
    cmd.env("NO_COLOR", "1");
    cmd
}

/// Assert that `s` contains no raw C0 (excluding `\t` and `\n`), DEL, or
/// C1 control codepoints.
///
/// The predicate iterates over *chars* (Unicode codepoints), not raw bytes,
/// so it correctly identifies C1 characters encoded as two-byte UTF-8
/// sequences (0xC2 0x80–0xC2 0x9F) without false-positives on continuation
/// bytes inside ordinary multi-byte codepoints.
///
/// # Panics
/// Panics on the first offending codepoint with a human-readable message that
/// includes `label`, the codepoint, its byte offset, and the full string.
#[allow(dead_code)]
pub fn assert_no_control_chars(s: &str, label: &str) {
    for (byte_offset, ch) in s.char_indices() {
        let code = ch as u32;
        let is_c0 = code < 0x20 && code != 0x09 && code != 0x0a;
        let is_del = code == 0x7f;
        let is_c1 = (0x80..=0x9f).contains(&code);
        assert!(
            !is_c0 && !is_del && !is_c1,
            "{label}: raw control char U+{code:04X} at byte offset {byte_offset}; \
             full string: {s:?}"
        );
    }
}
