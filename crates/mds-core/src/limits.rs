// ── Structural limits ─────────────────────────────────────────────────────────

/// Maximum number of segments in a dot-separated path (e.g. `a.b.c` = 3 segments).
/// Defense-in-depth limit independent of MAX_FILE_SIZE; half of the nesting cap.
pub(crate) const MAX_DOT_SEGMENTS: usize = 32;

/// Maximum nesting depth for @if/@for/@define blocks.
///
/// Prevents stack overflow from crafted inputs with deeply-nested blocks.
/// 64 levels is generous for any real template while keeping recursive parse
/// frames well within the 2 MB default thread stack on Linux/macOS (debug and
/// release builds).  256 required an 8 MB stack in tests; 64 does not.
pub(crate) const MAX_NESTING_DEPTH: usize = 64;

/// Maximum number of @elseif branches on a single @if block.
/// @elseif branches are flat (no stack frames), so 256 is safe independently of
/// MAX_NESTING_DEPTH (64), which limits recursive nesting depth.
pub(crate) const MAX_ELSEIF_BRANCHES: usize = 256;

// ── Size / traversal limits ───────────────────────────────────────────────────

/// Maximum file size (10 MB) to prevent runaway memory use.
///
/// Exported as `pub(crate)` so `src/lib.rs` can re-export it, and `fs.rs`
/// can import it for size checks on file reads.
pub(crate) const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum directory traversal depth when searching for project root markers.
///
/// Exported as `pub(crate)` so `src/lib.rs` can re-export it, and `fs.rs`
/// can import it for the `find_project_root` upward directory walk.
pub(crate) const MAX_TRAVERSAL_DEPTH: usize = 256;

// ── Pinning tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_have_expected_values() {
        assert_eq!(MAX_DOT_SEGMENTS, 32);
        assert_eq!(MAX_NESTING_DEPTH, 64);
        assert_eq!(MAX_ELSEIF_BRANCHES, 256);
        assert_eq!(MAX_FILE_SIZE, 10 * 1024 * 1024);
        assert_eq!(MAX_TRAVERSAL_DEPTH, 256);
    }
}
