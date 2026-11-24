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

/// Maximum size of an output buffer in bytes (50 MiB).
///
/// Checked by the evaluator before every append to an output buffer
/// (`push_capped`, #415) and by built-ins that can amplify output (e.g.
/// `replace()`) to prevent runaway memory use from adversarial inputs. Shared
/// with `builtins.rs` to ensure a single authoritative limit. It bounds each
/// buffer. `MAX_COMPILE_MEMORY` bounds the memory of the whole compile.
pub(crate) const MAX_OUTPUT_SIZE: usize = 50 * 1024 * 1024;

/// Maximum memory one compile may hold, in bytes (four output buffers: 200 MiB).
///
/// One budget per compile or check, kept in `evaluator::budget::MemBudget`. It counts
/// output buffers by capacity, the values that evaluation makes, the read text of each
/// module, the tree estimate of each module (see `TREE_PER_TOKEN`; an import scan
/// charges it against its own new budget), the compiler's copy of the caller variables, the text, tree estimates, frontmatter trees and scope layers
/// that cached modules keep, kept source-map segments, warnings, messages and the
/// finalize output. Each part is charged before its allocation, except at a few named
/// sites that charge right after one allocation. Exactly the budget passes. The next
/// charge fails with `mds::resource_limit` (`MdsError::compile_memory_exceeded`).
///
/// The compiler's copy of the caller variables is charged once per compile, as buffers,
/// at the root. The caller's own map is outside the budget. The count stops at 64 levels
/// of value nesting, so a Rust `Value` nested deeper fails this charge. On a root cache
/// hit, the copy is made and freed without a charge. On a reused `ModuleCache`, a copy
/// that a cached function captures lives after its compile, and only that compile counts
/// it.
///
/// Outside the budget: the lexer stage of each source (its `Vec<char>`, its
/// `byte_offsets` and the tokens it makes), which ends before the tree estimate is
/// charged. On a 64-bit host it holds 14 B per source byte for ASCII prose, and at most
/// 56 B per source byte for the densest syntax. Also outside: the caller's own variable
/// map, the line table, the YAML event buffer and binding output conversion. The
/// retained part of a reused `ModuleCache` never resets.
pub(crate) const MAX_COMPILE_MEMORY: usize = 4 * MAX_OUTPUT_SIZE;

/// Syntax tree bytes per token in the tree estimate.
///
/// The estimate of a module is `tokens * TREE_PER_TOKEN + expression bytes *
/// TREE_PER_EXPR_BYTE + source bytes * TREE_PER_TEXT_BYTE`, in checked arithmetic: a
/// count past the address space fails the charge. Expression bytes are the text of each
/// directive line and interpolation. The compile memory budget charges the estimate after
/// the lexer and before the parse of each module: a cached import, an `@extends` base,
/// the file entry and the string entry. An import scan (`scan_imports`,
/// `scan_import_records`) charges it in the same place against its own new budget. A
/// refused tree is never built. A cached module keeps its estimate charged. A skeleton
/// entry that is upgraded keeps one estimate.
///
/// The three factors are measured on a 64-bit host: for each of the 65 shapes in
/// `tests/memory_bound.rs`, the estimate is at least the compile peak and the bytes
/// that a cached module keeps. On wasm32 the estimate is high, so a compile refuses
/// early. For a module compiled alone, the largest sources that pass are about: 484,330 B
/// of the densest syntax (`\{{a`), 952,385 B of `{{a}}`, 616,935 B of the densest
/// measured expression shape and 5,229,805 B of prose with one `{{a}}` per 80 B. Plain
/// prose stops at the 10 MiB file cap first (the budget alone allows 12,336,188 B). A
/// reused `ModuleCache` fills faster: the sources of all modules that it caches in its
/// life share about the same ceilings (484,610 B, 956,729 B, 618,527 B, 5,237,969 B and
/// 12,336,188 B).
pub(crate) const TREE_PER_TOKEN: usize = 832;

/// Syntax tree bytes per byte of directive and interpolation text in the tree estimate.
pub(crate) const TREE_PER_EXPR_BYTE: usize = 184;

/// Syntax tree bytes per source byte in the tree estimate.
pub(crate) const TREE_PER_TEXT_BYTE: usize = 16;

/// Maximum total function calls per compile, across all evaluated modules.
/// Allows sixteen calls per pass in a million-pass loop budget.
pub(crate) const MAX_TOTAL_FUNCTION_CALLS: usize = 16_000_000;

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
/// realistic `key: value` block near 100 000; 200 000 rejects only amplification. The
/// node cap does not bound the bytes of the tree: an alias of a long scalar copies the
/// scalar at each node. The byte allowance of the parse bounds the tree: each node
/// spends `NODE_YAML` plus its string or key bytes, at most `MAX_OUTPUT_SIZE` per parse,
/// or the rest of the compile budget when that is smaller. Exceeding the node cap
/// surfaces as `mds::resource_limit`. See #162.
pub(crate) const MAX_FRONTMATTER_NODES: usize = 200_000;

/// Maximum running depth of flow-collection nesting (`[`/`{`) in one frontmatter
/// YAML block (1024).
///
/// Checked by `resolver::frontmatter::parse_frontmatter_yaml` in a single O(n) pass over
/// the raw bytes, AFTER the size cap and BEFORE the budgeted parse. libyaml's flow scanner
/// is O(depth^2) in flow-collection nesting, and that cost is paid inside the scanner
/// UPSTREAM of deserialisation. The three existing depth limits are all post-hoc: serde's
/// recursion limit (128 parse frames, "recursion limit exceeded"), and `Value::from_yaml_ref`'s
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

/// Maximum number of `@define` declarations per module.
///
/// This also bounds the number of capture links per module.
/// A module above the limit returns `mds::resource_limit`.
pub(crate) const MAX_DEFINES_PER_MODULE: usize = 256;

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
///
/// A compile reaches the compile memory budget (`MAX_COMPILE_MEMORY`) first: each
/// source byte costs at least 17 bytes of that budget, so a compile holds less than
/// this ceiling of sources. The ceiling stays as an extra guard.
pub(crate) const MAX_SOURCES_CONTENT_BYTES: usize = MAX_OUTPUT_SIZE;

/// Maximum number of distinct modules besides the entry in a compilation.
///
/// The resolver counts both cached keys and keys still being resolved, before
/// it reads each candidate. Binding input guards separately bound virtual maps,
/// including unused entries, to the entry plus this many modules.
pub(crate) const MAX_MODULE_COUNT: usize = 256;

/// Maximum number of aliases a `VirtualFs` takes (`VirtualFs::with_aliases`), and the
/// WASM `moduleAliases` option (#414).
///
/// An alias is another key an import reaches a module by: `@mdscript/mds`'s WASM
/// backend passes one for each spelling that reaches a module by other than its on-disk
/// key — a case variant on a case-insensitive volume, or a path through a symbolically
/// linked directory — so a project holds a handful. 65,536 is 256 such spellings for
/// each of 256 modules.
pub(crate) const MAX_MODULE_ALIASES: usize = 65_536;

/// Maximum total length in bytes of the aliases and the module keys they name in one
/// alias map: 10 MiB, the aggregate the WASM `modules` option allows its module text.
/// With `MAX_MODULE_ALIASES` it bounds what an alias map holds before any alias in it
/// is checked (#414).
pub(crate) const MAX_MODULE_ALIASES_SIZE: usize = MAX_FILE_SIZE as usize;

// ── Data source limits ────────────────────────────────────────────────────────

/// Maximum source bytes for one data file, checked during input admission.
/// A sparse file with long strings can reach this cap first. A dense file can
/// reach the node cap first, at about 1 to 2 MiB.
#[expect(dead_code, reason = "the data parser and readers land in later phases")]
pub(crate) const MAX_DATA_FILE_BYTES: u64 = MAX_FILE_SIZE;

/// Maximum nodes built by the parser for one data file.
/// A dense file can reach this cap at about 1 to 2 MiB. A sparse file with long
/// strings can reach the file byte cap first.
#[expect(dead_code, reason = "the data parser lands in a later phase")]
pub(crate) const MAX_DATA_NODES_PER_FILE: usize = 200_000;

/// Maximum distinct data files admitted in one compile, apart from modules.
/// Many small files can reach this cap first. Dense files can reach a node or
/// memory cap first. Large sparse files can reach a byte cap first.
#[expect(dead_code, reason = "data resolution lands in a later phase")]
pub(crate) const MAX_DATA_FILES: usize = 256;

/// Maximum data entries admitted during declaration analysis in one module.
/// This cap applies before file reads, for both dense and sparse data.
#[expect(dead_code, reason = "data declaration analysis lands in a later phase")]
pub(crate) const MAX_DATA_ENTRIES_PER_MODULE: usize = 64;

/// Maximum data entries admitted across modules in one compile.
/// This cap depends on declarations, not on whether the data is dense or sparse.
#[expect(dead_code, reason = "data resolution lands in a later phase")]
pub(crate) const MAX_DATA_ENTRIES_PER_COMPILE: usize = 256;

/// Maximum dependency levels in the data graph, checked before reads.
/// This cap depends on graph depth, not on whether the data is dense or sparse.
#[expect(dead_code, reason = "data graph analysis lands in a later phase")]
pub(crate) const MAX_DATA_LEVELS: usize = 16;

/// Maximum total source bytes admitted for distinct data files in one compile.
/// Large sparse files can reach this cap without reaching the node cap.
/// Dense data can reach the compile memory budget before the byte aggregate.
#[expect(dead_code, reason = "data resolution lands in a later phase")]
pub(crate) const MAX_DATA_BYTES_PER_COMPILE: usize = 32 * 1024 * 1024;
