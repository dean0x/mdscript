---
feature: mds-fmt
name: mds fmt — Opinionated Safety-Gated Formatter
description: "Use when modifying the mds fmt formatter engine (crates/mds-core/src/formatter.rs), the mds fmt CLI subcommand (crates/mds-cli/src/fmt.rs), any change to mds-core's output model (clean_output, evaluate_nodes, @message/@define body evaluation, the lexer's fence recognition) that could silently break the formatter's compile-equivalence guarantee, or changes to the shared directory walker (output.rs). Keywords: mds fmt, format_str, format_str_with, format_str_named, FormatterInvariant, clean_output, compile-equivalence, idempotent, assert_equivalent, structural_equivalent, strip_trailing_insignificant_text, in_raw_content, raw_content_spans, protected_spans, R1 R2 R3 R4, safety gate, token lossiness, @message body, @define body, @block body, FmtConfig, FmtFlags, interior-verbatim contract, try_scan_fence_at, FenceMatch, deep_merge_yaml, RESERVED_MERGE_KEYS, is_default_excluded_dir, is_within_default_excluded_dir, walker exclusions, node_modules, hidden dirs, effective_parent, bare filename, atomic_write_file."
category: domain-knowledge
directories: ["crates/mds-core/src", "crates/mds-cli/src"]
referencedFiles:
  - crates/mds-core/src/formatter.rs
  - crates/mds-core/src/lib.rs
  - crates/mds-core/src/evaluator.rs
  - crates/mds-core/src/lexer.rs
  - crates/mds-core/src/parser.rs
  - crates/mds-core/src/error.rs
  - crates/mds-core/src/resolver.rs
  - crates/mds-core/src/resolver/frontmatter.rs
  - crates/mds-core/src/fs.rs
  - crates/mds-cli/src/fmt.rs
  - crates/mds-cli/src/main.rs
  - crates/mds-cli/src/output.rs
  - crates/mds-cli/src/build.rs
  - crates/mds-cli/src/watch.rs
created: 2026-07-03
updated: 2026-07-19
---

# mds fmt — Opinionated Safety-Gated Formatter

## Overview

`mds fmt` (PR #137 / issue #60) rewrites `.mds` **source text** in place: `crates/mds-core/src/formatter.rs` (`format_str`/`format_str_named`, ~865 lines) is the engine, `crates/mds-cli/src/fmt.rs` is the CLI subcommand. It is CLI-only — as of this writing, no napi/wasm/python binding exposes `format_str_named` or `MdsError::FormatterInvariant` (verified: no match in `crates/mds-napi/src`, `crates/mds-wasm/src`, `crates/mds-python/src`). The public API surface is `format_str`, `format_str_with`, and `format_str_named` — all exported at `lib.rs:60` and pinned in `crates/mds-core/tests/api_surface.rs`.

What makes this feature different from a typical formatter: its correctness bar isn't an independent style guide, it's **whatever `mds-core`'s own compiled-output normalizer (`clean_output`) already does**. Every rule the formatter applies must be provable-safe against that normalizer's *exact* behavior — not its doc comment, its actual character-by-character logic — because a wrong rule doesn't just look ugly, it silently changes what an LLM receives at render time. All of the facts below were obtained by reading `clean_output` and `evaluator.rs` line-by-line and writing tests that pipe strings through the real compiler (`tests/fmt.rs` comments repeatedly say "verified empirically against the live compiler, not just inferred from its doc comment" — take that literally when touching this code: re-verify, don't assume the prose is still accurate).

This KB is domain-specific to the formatter's safety model. For the compiler pipeline it depends on (lexer/parser/resolver/evaluator, `CompileResult`, intrinsic output dispatch), see [[mds-compiler]]. For general CLI conventions (exit codes, `resolve_input`, `MAX_FILE_SIZE`, directory-mode machinery) it reuses, see [[mds-cli]]. **Neither of those KBs currently documents `clean_output`'s exact semantics or the `@message`/`@define` raw-body bypass — this file is the authoritative source for both**, even though they live in `mds-core`, not in the formatter.

## Business Context

MDS is a *content* language: whitespace in the compiled output is the product (Markdown hard breaks, message JSON payloads), not incidental formatting. That inverts the usual formatter assumption. The one non-negotiable business rule (stated in `formatter.rs`'s module doc, lines 1-13):

> Any tool that rewrites `.mds` source must produce byte-identical compile output (`compile(fmt(src)).output == compile(src).output`) and must be idempotent (`fmt(fmt(src)) == fmt(src)`).

This is enforced at **runtime**, per call, not just by test coverage — see "State Transitions" below. A future agent adding a new formatting rule who only adds unit tests (and skips understanding *why* the gate exists) is missing the point: the gate is what lets this ship at all despite the token stream being unable to prove correctness statically (see "Technical Implementation Patterns" below).

## Core Business Rules

### Rule 1 — `clean_output` is the ceiling on what the formatter may ever change

`clean_output` (`crates/mds-core/src/lib.rs:665-681`) is the compiler's own output normalizer, run on every markdown-mode body. Its exact logic, read directly from source (v0.4, post-interior-verbatim contract, PRs #150/#151):

```rust
pub(crate) fn clean_output(s: &str) -> String {
    // Strip \r unconditionally (Windows line endings).
    let no_cr: std::borrow::Cow<str> = if s.as_bytes().contains(&b'\r') {
        std::borrow::Cow::Owned(s.chars().filter(|&c| c != '\r').collect())
    } else {
        std::borrow::Cow::Borrowed(s)
    };

    // Trailing-edge normalisation: trim all trailing whitespace, then add one \n.
    let trimmed = no_cr.trim_end();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut out = trimmed.to_string();
    out.push('\n');
    out
}
```

**IMPORTANT CHANGE from v1**: The old `clean_output` (pre-#150/#151) had a `newline_count` loop that (a) stripped leading blank lines via `trim_start_matches` and (b) capped blank-line runs at `min(N, 2)`. Both behaviors are now **gone**. The current `clean_output` does exactly two things: (1) strip `\r` unconditionally and (2) a single `trim_end()` + forced final `\n`. It does NOT collapse interior blank lines, does NOT strip leading blank lines, and does NOT impose any structure on the body beyond "ends with exactly one newline." This is the interior-verbatim contract: the compiler passes block body content through verbatim (after CRLF normalization and final newline normalization), and the formatter must match that ceiling exactly.

**Whitespace contract at a glance (v0.4.0)** — three body-content paths, one deleted rule:

| Path | Compile-time treatment | Formatter treatment |
|---|---|---|
| Ordinary markdown-mode body **and** `@if`/`@for`/`@block` bodies | `clean_output` — trim-only / **interior-verbatim** (`\r` strip + trailing `trim_end` + one final `\n`) | R1 (`\r` strip) + R2 (final `\n`); interior blank runs copied verbatim |
| `@message` / `@define` bodies | **bypass `clean_output`** — edge-trimmed via `.trim()` only (spec §4.11) | raw-content spans — copied byte-for-byte (no R1, no R2), `\r` and interior blank runs both survive |
| ~~R3 blank-run collapse~~ | — | **DELETED** in v0.4.0 (#150/#151); no path caps blank-line runs anymore |

**Takeaways**: (1) the formatter must NOT add or remove blank lines anywhere in body content — interior blank lines are now the template author's exclusive domain; (2) the formatter must NOT strip trailing whitespace from a body-content line, including a whitespace-only one — two trailing spaces are a Markdown hard break; (3) `\r` is unconditionally deleted here, so removing it from the raw source up front is always safe, anywhere; (4) the old R3 rule (blank-run capping) has been **removed from the formatter** because `clean_output` no longer caps blank-line runs — applying it would change compiled output.

### Rule 2 — the THIRD protected region: `@message`/`@define` bodies bypass `clean_output` entirely

Frontmatter and code-fence content are the two "obviously protected" regions (they're reattached/copied verbatim around the parts of the pipeline that call `clean_output`). The formatter needed a third, non-obvious one, discovered by reading the evaluator rather than inferring it: `@message` body content is produced by (`crates/mds-core/src/evaluator.rs:845`, inside `collect_single_message`):

```rust
let content = evaluate_nodes(&block.body, scope, ctx)?.trim().to_string();
```

That's a plain `.trim()` — edge-trim once, per spec §4.11 — with **no `clean_output` call anywhere in this path**. Confirmed empirically (and locked in by `mds-core/tests/fmt.rs::message_body_carriage_return_is_preserved_not_stripped` and `::ec_blank_lines_inside_define_message_block_bodies_preserved`): `@message user:\r\nHi\r\nthere\r\n@end\r\n` compiles to message content `"Hi\r\nthere"` (the `\r` survives into the JSON), and 4 raw newlines inside a message body survive as 4. `@define` bodies get the identical conservative treatment — not because their evaluation bypasses `clean_output` (a function's own body content is subject to whatever call site renders it), but because a `@define`d function can be invoked from a markdown-mode site (where `clean_output` applies downstream) **or** from inside a `@message` body (where it does not), and the formatter has no call-graph analysis to tell which. So both `@message` and `@define` bodies are treated as **raw content**: copied byte-for-byte, no R1 (`\r` strip) applied, even to a code fence nested inside one (`formatter.rs::code_fence_nested_inside_message_body_is_fully_raw_not_just_r1_protected` locks this priority in).

`@if`/`@for`/`@block` do **not** bypass `clean_output` — they are **not** raw-content spans; only `@message`/`@define` bodies are. A standalone `@block` body therefore still flows through `clean_output` (it gets R1 `\r` strip + R2 trailing-edge normalization), whereas `@message`/`@define` bodies get **neither**. **Under the interior-verbatim contract (#150/#151), `clean_output` no longer caps blank-line runs, so a standalone `@block` body's *interior* blank runs are now preserved verbatim too** — the same observable blank-run behavior as `@message`/`@define`, reached by a different mechanism. This is locked in by `ec_blank_lines_inside_define_message_block_bodies_preserved` (`mds-core/tests/fmt.rs`), whose `@block instructions:\nStep one\n\n\n\nStep two\n@end\n` case now asserts the 4-newline run **survives** and recompiles identically. `@if`/`@for`/`@block` only inherit raw-content status when lexically nested inside a `@message`/`@define` span (the outer span's byte range structurally covers them). And critically, `@block` **cannot** nest inside a `@message` at all — the parser rejects it at parse time (`crates/mds-core/src/parser.rs:565-568`, one of three explicit top-level-only guards) — so a message body can never contain a nested `@block`.

**Region priority** (`rewrite_body`, `formatter.rs:382-507`), checked in this exact order per line: **directive line > raw-content (`@message`/`@define` body) > protected (frontmatter/code-fence) > ordinary content.** Directive lines win even inside a raw-content span because directive text (the `@message user:` line itself) is never part of *any* compiled output, markdown or messages mode — R4 always applies there.

### Rule 3 — the v2 safe ruleset (R1, R2, R4) and what was removed

| Rule | Effect | Why it's provably safe |
|---|---|---|
| **R1** | Strip every `\r`, including inside protected regions (the one rule allowed to cross that boundary) | `clean_output` strips `\r` unconditionally (Rule 1 above); frontmatter lexing already filters `\r` from its own captured content; directive matching trims `\r` before comparison. No downstream consumer of a `\r` byte can ever observe it, so removing it up front changes nothing. Not applied inside raw-content (`@message`/`@define`) spans. |
| **R2** | Exactly one final `\n`; whitespace-only/empty input → `""` | Matches `clean_output`'s `trim_end` + forced final `\n` exactly (`clean_output("   \n") == ""`, verified). |
| ~~**R3**~~ | ~~A run of N raw `\n` keeps `min(N, 2)`; leading blank lines at body start are elided entirely~~ | **REMOVED in v2 (#150, #151).** `clean_output` no longer caps blank-line runs or strips leading blank lines. Applying R3 would change compiled output under the interior-verbatim contract. |
| **R4** | Strip trailing whitespace on directive lines | The parser calls `dir.trim()` before matching a directive keyword, so trailing whitespace on that line is discarded pre-parse and never reaches any output — safe even inside a `@message`/`@define` body. |

**Deliberately deferred** (see `formatter.rs` module doc): frontmatter key sorting, `@import` grouping/reordering, interpolation `{ x }` → `{x}` trimming, blank-line insertion around directive blocks, body hard-break normalization, and directive-internal spacing.

**Deliberately NOT implemented**: stripping trailing whitespace from any whitespace-only "blank" line. The regression test `r4_whitespace_only_line_in_middle_of_document_is_preserved_verbatim` (`mds-core/tests/fmt.rs`) locks this in.

## State Transitions — the safety-gate decision tree

`assert_equivalent` (`formatter.rs:446-492`) is called once per `format_str_named` invocation, after the rewrite, and is the actual enforcement point:

1. **Original compiles standalone** (`compile_str_collecting_warnings(source, base_dir, None)` succeeds) → compile the *formatted* string the same way. Outputs equal → `Ok`. Outputs differ, or formatted now fails to compile → `Err(MdsError::FormatterInvariant)`. This is the strong path: a real recompile-and-diff.
2. **Original fails with `MdsError::Syntax`** → propagate that exact error, unconditionally. The `Syntax` error is rebuilt as `MdsError::Syntax { src: Some(NamedSource::new(file_name, source)) }` so the diagnostic names the file (using the `file_name` parameter threaded from `format_str_named`). This means an unclosed block or unmatched code fence is surfaced as the real author-facing syntax error, not misreported as `FormatterInvariant`. Locked in by `mds-core/tests/fmt.rs::syntax_error_unclosed_directive_blocks_surface_syntax_not_formatter_invariant`.
3. **Original fails with anything else** (undefined runtime variable/function is the common case) → the token stream is well-formed, only later analysis failed, so the template is still legitimately formattable. Falls back to `structural_equivalent` (`formatter.rs:565-615`): re-tokenize both strings, compare token-for-token using rule-aware normalization.

   **`structural_equivalent` post-v0.4.0 remediation** has two key behaviors worth knowing:

   - **`fmt_raw_content` is recomputed for the formatted stream** (line 575): `raw_content` uses SOURCE byte offsets, which are not valid against the FORMATTED token offsets. The function calls `raw_content_spans(&fmt_tokens, formatted)` to get a separate span vector for the formatted stream before doing the per-token comparison.
   - **`strip_trailing_insignificant_text`** (formatter.rs ~506-537) is called on **both** token streams before the length comparison (lines 580-581). It pops trailing `Text` tokens whose (a) offset is outside every raw-content span and (b) `crate::clean_output(text).is_empty()` — whitespace-only tokens that contribute nothing to compiled output. This prevents R2's `trim_end()` deletion of a trailing blank-line `Text` token from producing a spurious token-count mismatch when a trailing blank appears after a final directive in a non-compiling source (release blocker 3). Interior tokens are never touched by this helper, so real bugs (dropped interior content) still trip the count or content checks.

   Per-token comparison: `Text` tokens outside raw-content spans are compared via `crate::clean_output`; `Text` tokens inside raw-content spans are compared byte-exact (not via `clean_output`, which would incorrectly treat some byte differences as insignificant); `Directive` content compared after `.trim()`; Frontmatter/Code content compared after `\r` removal. Span membership via `in_raw_content` binary search (O(log S)).

## Technical Implementation Patterns

### Token lossiness forces a span-guided string rewriter, not an AST pretty-printer

Read directly in `crates/mds-core/src/lexer.rs`, the lexer permanently discards information a naive "reconstruct from tokens" formatter would need:

- **Interpolation inner whitespace is trimmed at lex time** — `Token::Interpolation(content.trim().to_string(), start)` (`lexer.rs:272`). `{ name }` and `{name}` produce the identical token; the original spacing is gone.
- **The newline after a directive line is consumed and never stored** — `scan_directive` (`lexer.rs:195-209`) advances `self.pos` past the line's own `\n` before pushing the `Directive` token.
- **The newline after a code fence line is likewise consumed** — `scan_code_fence` calls `skip_newline(...)` after both the opening and closing fence.
- **Frontmatter `\r` is filtered during lexing** — `FrontmatterContent`'s captured string has `\r` removed before it's even stored as a token (`lexer.rs:99`).

Because of this, **you cannot reconstruct source from tokens/AST without inventing or dropping whitespace the author actually wrote.** The formatter tokenizes *only* to classify byte ranges of the ORIGINAL source string, then rewrites that original string region-by-region in one left-to-right, O(n) pass, copying any byte it lacks a proven-safe rule for verbatim. It never builds output from token contents.

### `format_str_named` — the canonical public API

`format_str_named(source, base_dir, file_name)` (`formatter.rs:135`) is the canonical entry point. `format_str_with(source, base_dir)` is a thin wrapper passing `"<source>"` as the `file_name`. `format_str(source)` is a thin wrapper passing `None` for `base_dir`.

`file_name` is used in two places inside `format_str_named`:
1. **Lexer**: `lexer::tokenize(source, file_name)` — error spans in any `Syntax` error raised at the lex level will name the file.
2. **`assert_equivalent`'s Syntax arm** (path 2 above): when the original source fails with `MdsError::Syntax`, the error is rebuilt with `NamedSource::new(file_name, source)` so the miette diagnostic shows the correct filename (the internally-used blank label is replaced here).

The CLI calls it via the private `format_source_named` helper in `fmt.rs`, which passes:
- `path.display()` for single-file and directory mode
- `"<stdin>"` for stdin mode

In directory mode (`format_one_file`), both error branches (`read_source_file` failure and `format_source_named` failure) print `{file_name}: {error}`, so errors always name the file even without an explicit path in the miette span.

Pinned in `crates/mds-core/tests/api_surface.rs`: the test fixture checks the three-argument signature `fn(&str, Option<&Path>, &str) -> Result<String, MdsError>` so any signature change is caught at compile time.

### Shared walker exclusions (`is_default_excluded_dir`)

`is_default_excluded_dir` (`crates/mds-cli/src/output.rs:164`) and `is_within_default_excluded_dir` (`output.rs:177`) apply PF-004 (parallel-path enforcement) to directory traversal.

`is_default_excluded_dir(name)` returns `true` when a **directory** name starts with `'.'` (hidden dirs like `.git`, `.cache`) or equals `"node_modules"`. Exclusion semantics:

- **Recursion only**: the root directory explicitly passed to `collect_mds_files` is always processed, even if its own name starts with `.`. Hidden *files* (e.g. `.dotfile.mds`) at any traversed level are still collected.
- **All subcommands**: `run_fmt_directory`, `run_build_directory`, `run_check_directory`, and `run_lint_directory` all call `collect_mds_files` from `output.rs` — the exclusion is inherited automatically. This is what closes the "fmt writes into node_modules" bug: `collect_mds_files` never yields those paths, so `run_fmt_directory` never visits them.

`is_within_default_excluded_dir(root, path)` checks whether a `path` is *inside* a default-excluded subdirectory of `root` — used by watch's two additional guards (PF-004: the limit must be enforced on the parallel event-processing path too):

1. **Event drop** (`handle_fs_event_dir`, `watch.rs:1547`): `changed.retain(|p| !is_within_default_excluded_dir(&ctx.root, p))` — filesystem events from excluded dirs are dropped before they trigger a rebuild. Without this, `npm install` writing to `node_modules/` would trigger spurious full re-scans.
2. **External-dep treatment** (`process_dir_batch`, `watch.rs:2053`): paths inside excluded subdirs that are nonetheless under root are compiled for dep-graph refresh only (quiet mode, no output emitted), matching the initial walker's behavior.

### Fence recognition widened in #149 — a strict superset of backtick fences

`try_scan_fence_at` (`lexer.rs:409-439`) and its `FenceMatch` struct (`lexer.rs:396-400`) define what the lexer counts as a code fence. As of #149 it scans an **optional `[ \t>]*` prefix** and then requires **≥ 3 consecutive `` ` `` *or* `~`** markers. This makes it recognize indented fences, tilde fences, and blockquoted fences. All of these produce `CodeContent` tokens (interpolation suppressed) and become `protected_spans`. A stray/decorative fence-looking line that never closes now raises `MdsError::syntax_at("unclosed code fence")` (`lexer.rs:371-379`) instead of tokenizing as plain text — in `mds fmt` that lands in `assert_equivalent`'s Syntax path (path 2 above) and is propagated as the author's real error.

### CLI integration (`crates/mds-cli/src/fmt.rs`)

Notable divergences worth knowing before touching this file:

- **`FmtFlags { check, diff, quiet }`** (`fmt.rs:43-48`) is a named struct passed to every per-input helper. Avoids the silent transposition hazard; callers destructure it explicitly.
- **`format_one_file(file, flags) -> FileOutcome`** (`fmt.rs:232-282`) is a helper that encapsulates the read → format → diff → (optional write) cycle for a single file in directory mode. Error and status lines are printed as side effects so the directory loop only tallies `FileOutcome`.
- **`MAX_DEPTH = 64`** is declared as a function-local constant inside `run_fmt_directory` (line 297), matching `run_build_directory`/`run_check_directory`.
- **Raw-text read**: `read_source_file` (`fmt.rs:110-118`) cannot go through `mds::compile*`. It reuses `mds::NativeFs::check_symlink` for the TOCTOU-safe canonicalize+symlink check.
- **Channel discipline**: formatted content (stdin filter mode) and `--diff` output go to **stdout**; every status line, summary, and error goes to **stderr**; `--quiet` suppresses status/summaries but never errors.
- **`--check` directory summary** is `"{changed_count} would reformat, {unchanged_count} unchanged, {fail_count} failed"` (`fmt.rs:335`). The `changed_count > 0` disjunct in the summary print was removed; only `fail_count > 0` forces a summary under `--quiet`.
- **`--diff`** renders via the `similar` crate's unified diff, ANSI-colorized only when `std::io::stdout().is_terminal()`. The colorizer (`colorize_unified_diff`, `fmt.rs:396-430`) uses an `in_hunk` state-machine flag set on the first `@@` marker, correctly handling content lines that themselves start with `---` or `+++`.
- **Write-only-if-changed** preserves mtime on already-clean files.
- **Directory mode formats partials** (`_`-prefixed files) — a deliberate divergence from `run_build_directory`/`run_check_directory`, which skip them. Formatting rewrites *source*, and a partial's source is just as reformattable as any other file.
- **`FmtConfig { sort_frontmatter_keys: bool }`** (`build.rs:36-65`) is forward-compat scaffolding: parsed and validated but **not consulted by any formatting behavior** — a deliberate no-op.
- **After-help stdin demo** (`main.rs`) uses `printf '@if ready:   \nGo\n@end\n' | mds fmt -`. The trailing spaces on the directive line are stripped by R4, so this IS input that `mds fmt` actually changes — verified that after_help was updated from a no-op example.

## Error Handling and Recovery

`MdsError::FormatterInvariant { message: String }` (`error.rs:335-345`, code `mds::formatter_invariant`, constructed via the private `MdsError::formatter_invariant()` helper at `error.rs:686-692`) means **the formatter itself has a bug** — the CLI must never write the file when this occurs. The field name is `message`, matching all other free-form-string variants in `MdsError`. Both `FormatterInvariant` and `Syntax` map to the generic `_ => 1` arm in `exit_code` (`build.rs:372-382`) — no new exit codes were added for `fmt`. The CLI-level contract: `format_str_named` returns `Err`, never a garbled `Ok(String)`, so `fmt.rs` only ever reaches its write call with a value that already passed the gate.

## Anti-Patterns

- **Reconstructing formatted output from tokens or the AST.** The lexer is lossy — this will silently invent or drop whitespace the author wrote.
- **Stripping whitespace from any line that "looks blank."** A whitespace-only line in the middle of a document is body content — two trailing spaces are a Markdown hard break.
- **Adding blank-line collapsing (R3) back to the formatter.** R3 was removed in v2 (#150/#151) because `clean_output` no longer caps blank-line runs. This applies to `@block` bodies too.
- **Applying R1 (`\r` strip) inside a `raw_content_spans` range.** `@message`/`@define` bodies bypass `clean_output` entirely — removing `\r` from them would change compiled JSON content.
- **Treating `structural_equivalent` as equivalent in strength to a real recompile.** It's a token-comparison approximation used only when the source doesn't compile standalone. Never use it as a substitute for path 1.
- **Using `raw_content` (source offsets) to check tokens from the formatted string.** `structural_equivalent` recomputes `fmt_raw_content` from the formatted token stream because source byte offsets are not valid against formatted-string token offsets.
- **Adding a new formatting rule without empirically re-verifying `clean_output`/`evaluate_nodes` behavior first.** The doc comment on `clean_output` is not authoritative — read the source code.
- **Assuming `@define` bodies are safe to collapse because they look like ordinary markdown-mode content.** They're conservatively treated as raw because the formatter cannot statically determine whether a given `@define`d function is ever called from inside a `@message` body.

## Gotchas

- **`@block` bodies now preserve interior blank runs.** A standalone `@block` body is NOT a raw-content span (it still goes through `clean_output`, unlike `@message`/`@define`), but because `clean_output` is now interior-verbatim, its interior blank runs survive. Do not "fix" a `@block` that keeps a 4-newline run — that is now correct behavior.
- **`effective_parent` (fs.rs ~287) fixes bare-filename resolution everywhere.** `Path::parent()` returns `Some("")` (an empty path) for bare relative filenames like `"hello.mds"`, NOT `None`. Passing an empty path to `canonicalize` produces a file-not-found error. `effective_parent` maps both `Some("")` and `None` to `Path::new(".")`, so bare filenames resolve from the current working directory. This was release blocker 1 (before the fix, `mds fmt hello.mds` with no directory prefix would fail). `check_symlink` explicitly calls `effective_parent` in its parent-directory step. `atomic_write_file` also calls `effective_parent` for the same reason.
- **`strip_trailing_insignificant_text` is tail-only, not interior.** The helper only pops from the END of the token stream. An interior blank-line `Text` token (e.g. after a `@if` block in the middle of a file) is never touched. A false `FormatterInvariant` that triggers for a trailing blank line after a final directive (on a source that only compiles with runtime vars) was release blocker 3 — fixed by this helper.
- **Frontmatter deep-merge (`@extends`, #153) lives outside `formatter.rs` but shares the reserved-key vocabulary.** `deep_merge_yaml` is in `crates/mds-core/src/resolver/frontmatter.rs` (not `resolver.rs`), and its reserved-key strip (`RESERVED_MERGE_KEYS = ["imports", "type", "extends"]`) is gated on **`depth == 0`**. The raw frontmatter block is copied verbatim by the formatter, so `sort_frontmatter_keys` staying a no-op keeps the formatter clear of this logic.
- **FIXED (PF-003 / #133, #146): the resolver's `<source>` path-sentinel is eliminated.** `resolve_source`/`resolve_source_intrinsic` now pass the canonical directory string directly as `ctx.base_dir` and call `fs.normalize_in_dir(ctx.base_dir, rel)` — no synthetic path constructed. `SOURCE_LABEL` const is display/cycle-detection only. Locked in by `resolver_tests.rs::pf003_parent_dir_strips_on_windows_verbatim_path`.
- **`FmtConfig.sort_frontmatter_keys` parses but does nothing.** Don't "wire it up" opportunistically.
- **Adding a field to `MdsConfig` ripples into unrelated existing tests.** The `fmt: FmtConfig` field addition required `..Default::default()` in pre-existing struct-literal test sites in `build.rs` and `watch.rs`.
- **Partials are reformatted but `is_partial` still gates output emission elsewhere.** Don't conflate the two meanings of "partial" across `fmt` vs. `build`/`check`.
- **Hidden FILES are still collected by the walker.** `is_default_excluded_dir` excludes hidden *directories* from recursion, not hidden `.mds` files at the traversed level. A `.dotfile.mds` at the root of a traversed directory is collected and formatted.
- **`atomic_write_file` is the write primitive — not `std::fs::write`.** `fmt.rs` calls `atomic_write_file` from `output.rs` (shared with `lint.rs`). It provides a TOCTOU guard, Unix permission preservation (`mode & 0o7777`), and `sync_all()` + atomic rename. This means a failed write leaves the original file intact — it does NOT leave a partially-written file.

## Deferred follow-ups (recorded to avoid re-flagging as new debt)

These items were consciously deferred and have back-ref comments in source:

- **A1 — block-opener keyword const centralization**: `raw_content_spans` in `formatter.rs` and the parser's `parse_block` both contain the same list of block-opener keywords inline. Extracting a shared const was deferred (requires deciding where in the crate the const lives without a circular module dependency). Still deferred as of 2026-07-19.
- **R3 — `write_stderr` broken-pipe helper**: the `write_stdout` helper in `fmt.rs` handles broken pipe cleanly, but equivalent stderr writes still use bare `eprintln!`. Still deferred as of 2026-07-19.
- **End-to-end `FormatterInvariant` gate test**: a test that forces `assert_equivalent` to return `FormatterInvariant` at the CLI level (a deliberate formatter bug scenario) remains deferred. See `cli_fmt.rs:131` for the comment. The new `strip_trailing_insignificant_text` fix for release blocker 3 is tested (`cli_fmt.rs:992`) — that test verifies the false-positive is gone, not the true-positive path.
- **D1 — systematic license attribution**: no per-file SPDX headers yet; deferred pending a crate-wide audit pass.
- **D2 — cargo-deny / cargo-audit CI gate**: not yet added to release or PR CI.
- **S1 — openat/O_NOFOLLOW write-path hardening**: `fmt.rs` now calls `atomic_write_file` (from `output.rs`), which is atomic (TOCTOU guard + `effective_parent` + temp-file rename). The "not atomic" sub-item is resolved. However, `O_NOFOLLOW` is still not used for the actual write file descriptor — the read-side `NativeFs::check_symlink` provides partial mitigation. The remaining gap is the write-side file descriptor; `openat` / `O_NOFOLLOW` hardening is still deferred.

## Key Files

- `crates/mds-core/src/formatter.rs` — the entire engine: `format_str_named` (~line 135), region computation, the rewrite pass, `strip_trailing_insignificant_text` (~line 522), `in_raw_content` binary-search helper, `assert_equivalent` (~line 446), and `structural_equivalent` (~line 565)
- `crates/mds-core/src/lib.rs:60` — `pub use formatter::{format_str, format_str_named, format_str_with}` (the public re-export); `:665-681` — `clean_output`
- `crates/mds-core/src/fs.rs:287` — `effective_parent` (bare-filename fix, maps `Some("")`/`None` → `"."`)
- `crates/mds-cli/src/output.rs:164,177` — `is_default_excluded_dir` / `is_within_default_excluded_dir` (shared walker exclusions; applies to all subcommands and both watch paths); `:432` — `atomic_write_file` (shared write primitive: TOCTOU guard + permissions restore + sync_all + atomic rename; used by both `fmt.rs` and `lint.rs`)
- `crates/mds-core/src/evaluator.rs:845` — the `evaluate_nodes(...).trim()` call (spec §4.11 edge-trim) that makes `@message` bodies bypass `clean_output`
- `crates/mds-core/src/resolver/frontmatter.rs:53-129` — `deep_merge_yaml` (`@extends` frontmatter merge)
- `crates/mds-core/src/parser.rs:545-594` — `parse_block`; the `@block`-cannot-nest-in-`@message` guard
- `crates/mds-core/src/lexer.rs` — token-lossiness sites and fence recognition (`try_scan_fence_at`/`FenceMatch`, `fence_closes`, unclosed-fence error)
- `crates/mds-core/src/error.rs:335-345, 686-692` — `FormatterInvariant { message: String }`
- `crates/mds-cli/src/fmt.rs` — the `fmt` subcommand: `FmtFlags`, `format_source_named` calling `mds::format_str_named`, `format_one_file`, channel discipline, diff colorizer; calls `atomic_write_file` (not `std::fs::write`)
- `crates/mds-cli/src/build.rs:16-65` — `MdsConfig`/`FmtConfig`/`BuildConfig`; `:372-382` — `exit_code`
- `crates/mds-core/tests/api_surface.rs:14` — compile-time pin of `format_str_named` signature
- `crates/mds-core/tests/fmt.rs`, `crates/mds-cli/tests/cli_fmt.rs` — every empirically-verified claim above is locked in as a regression test here

## Related

- [[mds-compiler]] — the compilation pipeline the formatter's safety gate re-invokes (`compile_str_collecting_warnings`, `CompileResult`, intrinsic Markdown/Messages dispatch).
- [[mds-cli]] — command dispatch, exit-code mapping, `resolve_input`/`auto_detect_mds_file`, directory-mode machinery (`collect_mds_files`, `MAX_DEPTH`), and `MAX_FILE_SIZE` enforcement.
- ADR-001 (compile-equivalence gate) — the architectural decision behind `assert_equivalent`: every formatter call must produce byte-identical compiled output, enforced at runtime per call, not just by test coverage.
- ADR-002 (verbatim whitespace contract) — the architectural decision behind the removal of R3: `clean_output` is interior-verbatim, so the formatter must pass interior blank lines through unchanged.
- PF-004 (parallel-path enforcement) — the pitfall behind `is_default_excluded_dir` appearing in both `collect_mds_files_inner` and watch's `handle_fs_event_dir` / `process_dir_batch`: the same limit must be enforced on every path (initial walker and event-processing path) or events from excluded dirs slip through to trigger spurious rebuilds.
- PF-003 — **RESOLVED** (PRs #133/#146): Windows verbatim-path `@import` pitfall in the resolver. The `<source>` path-sentinel is eliminated; `resolve_source`/`resolve_source_intrinsic` now use `FileSystem::normalize_in_dir`. No longer in the active pitfalls set.
- `.devflow/features/mds-lint/KNOWLEDGE.md` — `mds lint` knowledge base; `--fix` write path shares `atomic_write_file` from `output.rs`.
- `.devflow/features/source-map-security/KNOWLEDGE.md` — source map path-containment choke-point (`relativize_source`), `FileSystem::source_root()`, `CompileOptions.source_map_base`. Not directly relevant to `mds fmt` today (fmt is CLI-only and does not emit source maps), but relevant if source-map output is ever added to `mds build`.
