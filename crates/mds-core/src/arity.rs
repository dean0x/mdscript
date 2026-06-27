/// Arity checking helper — shared by the evaluator and the validator.
///
/// Returns `true` when `provided` is within the `[min, max]` range
/// (inclusive on both sides), `false` otherwise.
///
/// # Design note
///
/// This function deliberately performs only the comparison and returns a plain
/// `bool`.  It does **not** construct any `MdsError`.  Each call site keeps its
/// existing error variant:
///
/// - `evaluator` → `MdsError::arity` (no source span)
/// - `validator` → `MdsError::arity_at` (carries the source span)
///
/// Keeping error construction at the call site preserves the divergence between
/// the two error kinds without introducing conditional logic or parametric
/// complexity in this helper.
#[inline]
pub(crate) fn check_arity(provided: usize, min: usize, max: usize) -> bool {
    provided >= min && provided <= max
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_range_returns_true() {
        assert!(check_arity(1, 1, 1));
        assert!(check_arity(0, 0, 0));
        assert!(check_arity(2, 1, 3));
        assert!(check_arity(3, 1, 3));
    }

    #[test]
    fn below_min_returns_false() {
        assert!(!check_arity(0, 1, 2));
    }

    #[test]
    fn above_max_returns_false() {
        assert!(!check_arity(3, 1, 2));
    }

    #[test]
    fn exact_min_and_max_boundaries() {
        // min == max (exact arity)
        assert!(check_arity(1, 1, 1));
        assert!(!check_arity(0, 1, 1));
        assert!(!check_arity(2, 1, 1));
    }
}
