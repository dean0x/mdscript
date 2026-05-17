/// Maximum number of segments in a dot-separated path (e.g. `a.b.c` = 3 segments).
/// Defense-in-depth limit independent of MAX_FILE_SIZE; half of the nesting cap.
pub(crate) const MAX_DOT_SEGMENTS: usize = 32;
