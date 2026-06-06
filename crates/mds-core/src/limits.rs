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

/// Maximum number of leaf operands in a single `&&` or `||` expression.
///
/// Prevents adversarial inputs from creating exponentially-evaluated condition
/// trees. 16 operands allows complex but realistic conditions.
pub(crate) const MAX_LOGICAL_OPERANDS: usize = 16;

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

/// Maximum size of the compiled output string in bytes (50 MB).
///
/// Checked by the evaluator after each node and by built-ins that can amplify
/// output (e.g. `replace()`) to prevent runaway memory use from adversarial
/// inputs. Shared with `builtins.rs` to ensure a single authoritative limit.
pub(crate) const MAX_OUTPUT_SIZE: usize = 50 * 1024 * 1024;

/// Maximum number of elements that `split()` may produce in a single call.
///
/// Prevents adversarial inputs from producing arrays with hundreds of thousands
/// of elements that could exhaust memory during subsequent `@for` iteration or
/// `join()` calls. 100 000 elements is generous for any real template while
/// bounding worst-case memory use.
pub(crate) const MAX_ARRAY_ELEMENTS: usize = 100_000;

/// Maximum number of `imports` entries in frontmatter.
///
/// Defense-in-depth limit preventing adversarial inputs from triggering
/// an unbounded number of file resolutions in a single frontmatter block.
/// 256 entries is generous for any real template.
pub(crate) const MAX_FRONTMATTER_IMPORTS: usize = 256;
