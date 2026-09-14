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

/// Maximum byte length of one frontmatter YAML block (1 MiB).
///
/// Checked by `resolver::frontmatter::parse_frontmatter_yaml` before any YAML work: the
/// `serde_yaml_ng` loader is eager (it drains the whole document into an event vector
/// before deserialising), so the cap must sit in front of it. Frontmatter is variable
/// data, not prose: 1 MiB is on the order of 50 000 `key: value` lines, while the body
/// keeps the 10 MiB `MAX_FILE_SIZE` bound. Exceeding this surfaces as
/// `mds::resource_limit` (CLI exit 3). See #162.
pub(crate) const MAX_FRONTMATTER_SIZE: usize = 1024 * 1024;

/// Maximum number of YAML nodes one frontmatter block may materialise (200 000).
///
/// Counted while `serde_yaml_ng` deserialises (every scalar, null, sequence, mapping,
/// mapping key and `!tag` wrapper is one node), so the parse fails before the tree is
/// built. This is the alias bound: an `&anchor` referenced by many `*alias`es expands at
/// deserialise time, so a block under `MAX_FRONTMATTER_SIZE` could otherwise demand on
/// the order of size^2/24 nodes (about 4 x 10^10 for 1 MiB). `serde_yaml_ng`'s own
/// repetition limit counts alias jumps, not nodes, and does not catch one large anchor
/// referenced a few thousand times. An alias-free block needs at least 2 bytes per node
/// (`[x,x,...]`), so under the size cap it stays around 525 000 nodes at most and a
/// realistic `key: value` block near 100 000; 200 000 rejects only amplification and
/// bounds the materialised tree at a few tens of MB per parse. Exceeding this surfaces
/// as `mds::resource_limit`. See #162.
pub(crate) const MAX_FRONTMATTER_NODES: usize = 200_000;

/// Maximum running depth of flow-collection nesting (`[`/`{`) in one frontmatter
/// YAML block (1024).
///
/// Checked by `resolver::frontmatter::parse_frontmatter_yaml` in a single O(n) pass over
/// the raw bytes, AFTER the size cap and BEFORE the budgeted parse. libyaml's flow scanner
/// is O(depth^2) in flow-collection nesting, and that cost is paid inside the scanner
/// UPSTREAM of deserialisation. The three existing depth limits are all post-hoc: serde's
/// recursion limit (128 parse frames, "recursion limit exceeded"), and `Value::from_yaml`'s
/// `MAX_VALUE_DEPTH` (64, "value nesting exceeds maximum depth of 64"). Every one of them
/// fires only AFTER the quadratic scan has already been paid, so a ~1 MiB pure deep
/// flow-nest (no anchors) burns 10+ s of CPU at ~32 MB RSS before any of them rejects it —
/// and the node budget cannot catch it (few nodes, trivial memory). A cheap pre-parse bound
/// on flow-nesting depth is the only thing that stops it before the scanner runs.
///
/// 1024 is far above any legitimate frontmatter — flow collections are never nested even
/// 100 deep — yet it caps the worst admitted scan at ~1024^2 work (trivially fast). It sits
/// deliberately ABOVE serde's 128-frame recursion limit so the parser's own
/// recursion/value-depth errors stay reachable and unchanged for shallower inputs.
/// Block-style (indent) nesting has no quadratic cost and is not counted. Exceeding this
/// surfaces as `mds::resource_limit`. See #162.
pub(crate) const MAX_FRONTMATTER_FLOW_DEPTH: usize = 1024;

/// Maximum number of messages a `@message`-bearing template may produce.
///
/// Prevents runaway memory use from adversarial inputs that generate thousands
/// of messages via `@for` loops or deeply nested conditionals.
/// 10 000 messages is generous for any real LLM conversation template.
pub(crate) const MAX_MESSAGE_COUNT: usize = 10_000;

/// Maximum number of `@block` declarations per module.
///
/// Defense-in-depth limit preventing adversarial inputs from triggering
/// unbounded name-collision checks in `collect_block`. 256 blocks is generous
/// for any real template.
pub(crate) const MAX_BLOCKS_PER_MODULE: usize = 256;

/// Maximum recursion depth for `deep_merge_yaml` when merging frontmatter
/// Mappings across template inheritance chains.
///
/// Prevents stack overflow from adversarially-crafted deeply-nested YAML
/// objects in frontmatter. 64 levels is generous for any real template while
/// keeping recursive frames well within the default thread stack.
/// Exceeding this limit surfaces as `mds::resource_limit` (P4).
pub(crate) const MAX_FRONTMATTER_MERGE_DEPTH: usize = 64;

/// Maximum number of lint diagnostics collected per file.
///
/// When the accumulated diagnostic count reaches this limit, collection stops and
/// `LintResult::truncated` is set to `true`. The truncation marker tells callers to
/// re-run after addressing visible findings (especially in `--fix` mode, where
/// remaining diagnostics may be revealed on subsequent passes).
///
/// Re-exported as a public constant via `lib.rs` so bindings and tests can pin its
/// value (L-API-5 / AC-API-10).
pub(crate) const MAX_DIAGNOSTICS: usize = 1_000;

/// Maximum cumulative byte size of all message content produced by a `@message`-bearing template.
///
/// Caps the aggregate content across the entire message array at the same ceiling as
/// a single text-mode output (MAX_OUTPUT_SIZE = 50 MB).  Without this, 10 000 messages
/// each up to MAX_OUTPUT_SIZE (50 MB) could collectively allocate ~500 GB in a single
/// evaluation.  Individual message bodies are already bounded by MAX_OUTPUT_SIZE via the
/// `evaluate_nodes` size check; this limit guards the cumulative total.
/// The incremental check in `collect_single_message` catches runaway growth early.
pub(crate) const MAX_MESSAGES_TOTAL_SIZE: usize = MAX_OUTPUT_SIZE;

/// Maximum number of segments in a generated source map.
///
/// At 16 bytes per `RawSegment`, 1 000 000 segments cap the in-memory segment buffer
/// at ~16 MiB — well above the segment count for any practical MDS template (a 50 MiB
/// output would need ~millions of distinct tokens to saturate this limit).  New segments
/// beyond the cap are silently dropped so compilation succeeds with a partial map rather
/// than erroring on adversarial inputs.
pub(crate) const MAX_SOURCEMAP_SEGMENTS: usize = 1_000_000;

/// Maximum total byte size of all `sourcesContent` strings embedded in a source map.
///
/// Guards against unbounded artifact size when a template imports many large files.
/// When the total bytes of source file contents would exceed this ceiling, the
/// `sourcesContent` array is omitted (degraded) rather than embedding the full corpus.
/// The source map remains valid — consumers will fetch sources separately.
///
/// Set equal to `MAX_OUTPUT_SIZE` (50 MB): the content set should not dwarf the
/// compiled output it annotates.
pub(crate) const MAX_SOURCES_CONTENT_BYTES: usize = MAX_OUTPUT_SIZE;

/// Maximum number of distinct modules (files) that may be resolved in a single
/// compilation, across all paths (native file, virtual/WASM/napi).
///
/// Enforced on the native file resolution path by `resolve_by_key` so that
/// adversarial import graphs cannot cause unbounded module loading.  The
/// virtual/WASM/napi binding layers enforce the same cap at input-validation time
/// before any resolution begins (applies PF-004).
///
/// 256 matches the binding-layer constant so all code paths behave identically.
pub(crate) const MAX_MODULE_COUNT: usize = 256;
