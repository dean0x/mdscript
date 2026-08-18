# markdown-script

Native **Python bindings** for [MDS (Markdown Script)](https://github.com/dean0x/mdscript) —
a composable LLM prompt-template compiler. Compile `.mds` templates to Markdown or
structured chat messages in-process, backed by the same Rust core as the MDS CLI and
the Node.js / WASM bindings. Output is byte-identical across every binding.

```bash
pip install markdown-script
```

> **Not yet on PyPI** — publishing and the `markdown-script` name registration are tracked in
> [#132]. For now, build from source: `pip install ./crates/mds-python` (or `maturin
> build -m crates/mds-python/Cargo.toml` to produce a wheel), with a Rust toolchain and
> `python3` on `PATH`. Once published, wheels ship as `cp311-abi3` (CPython 3.11+, one
> wheel per platform).

[#132]: https://github.com/dean0x/mdscript/issues/132

## Quick start

```python
import markdown_script

# Markdown template
r = markdown_script.compile("Hello {{name}}!", vars={"name": "Alice"})
assert r.kind == "markdown"
assert r.output == "Hello Alice!"

# @message template → structured messages
r = markdown_script.compile("@message user:\nHi\n@end\n")
assert r.kind == "messages"
assert r.messages[0].role == "user"
assert r.output is None            # inactive payload is None

# Validate without rendering
markdown_script.check("Hello {{name}}!", vars={"name": "Bob"})

# Compile a file (dependencies come back as absolute paths)
r = markdown_script.compile_file("prompts/agent.mds")
print(r.dependencies)
```

## API

All compile/check functions return a typed, picklable result. Keyword arguments are
keyword-only; `scan_imports` takes its argument positionally.

| Function | Signature |
|----------|-----------|
| `compile` | `compile(source, *, vars=None, base_path=None, source_map=False, sources_content=False) -> CompileResult` |
| `compile_file` | `compile_file(path, *, vars=None, source_map=False, sources_content=False) -> CompileResult` |
| `compile_virtual` | `compile_virtual(modules, entry, *, vars=None, source_map=False, sources_content=False) -> CompileResult` |
| `check` | `check(source, *, vars=None, base_path=None) -> CheckResult` |
| `check_file` | `check_file(path, *, vars=None) -> CheckResult` |
| `check_virtual` | `check_virtual(modules, entry, *, vars=None) -> CheckResult` |
| `scan_imports` | `scan_imports(source, /) -> list[str]` |
| `lint` | `lint(source, *, vars=None, base_path=None, rules=None) -> LintResult` |
| `lint_file` | `lint_file(path, *, vars=None, rules=None) -> LintResult` |
| `lint_virtual` | `lint_virtual(modules, entry, *, vars=None, rules=None) -> LintResult` |

- `path` / `base_path` accept `str` or `os.PathLike`.
- `vars` is a mapping of string keys to JSON-compatible values; a non-mapping raises
  `MdsError(code="mds::invalid_options")`.
- `compile_virtual` / `check_virtual` / `lint_virtual` resolve imports against an in-memory
  map; `entry` must be a key in `modules`.
- `source_map=True` generates a Source Map v3 document; `result.source_map` is a `dict`.
  For string-source compiles `sources[0]` is `"input.mds"`. `sources_content=True` embeds
  the original source text in `sourcesContent[]` (requires `source_map=True`).
  ⚠ Privacy: `sources_content=True` embeds the full template source in the map.
- `rules` is a mapping of rule name → severity string (`"off"`, `"info"`, `"warn"`, `"error"`).
  Unknown severity values raise `MdsError(code="mds::invalid_options")`; unknown rule names
  emit a warning and lint continues — the unknown name has no effect, but a non-empty
  `result.lint_warnings` list signals the problem so callers can surface it.
  `LintResult` exposes `.version`, `.truncated`, `.lint_warnings`, `.to_dict()`, `.to_json()`, and `.files`
  — a `list[LintFileReport]`. Each `LintFileReport` has `.file` (`str`) and `.diagnostics`
  (`list[LintDiagnostic]`). `LintDiagnostic` carries `.rule`, `.severity`, `.message`,
  `.help` (`str | None`), `.fixable` (`bool`), `.fix_edits` (`list[dict] | None`), and `.span` (`Span | None`).
  `LintFileReport` and `LintDiagnostic` are frozen, picklable, and comparable by value.

### Result objects

`CompileResult` exposes `.kind` (`"markdown"` | `"messages"`), `.output` (`str | None`),
`.messages` (`list[Message] | None`), `.warnings`, `.dependencies`, and `.source_map`
(`dict | None`). `CheckResult` exposes `.warnings`. Both offer `.to_dict()` and `.to_json()`.

> **`to_dict()` vs `to_json()` asymmetry (source maps):** `CompileResult.to_dict()` always
> includes `"sourceMap": None` when no source map was generated — Python-idiomatic
> always-present. `to_json()` omits the key when absent, matching the canonical wire format
> shared with the CLI, napi, and WASM surfaces for byte-identical cross-surface parity.

Results are frozen, comparable by value, intentionally unhashable, and picklable.

### Errors

Every failure raises `markdown_script.MdsError` (a subclass of `Exception`):

```python
try:
    markdown_script.compile("Hello {{undefined}}!")
except markdown_script.MdsError as e:
    print(e.code)          # "mds::undefined_var"
    print(str(e))          # == e.message
    print(e.help)          # hint, or None
    if e.span:
        print(e.span.line, e.span.column)   # 1-indexed
```

## Concurrency

Compilation is synchronous, stateless CPU work and **releases the GIL**, so calls
parallelise across threads. For `asyncio`, offload with `asyncio.to_thread(markdown_script.compile, src)`.
The extension is also free-threading (`cp314t`) ready — result classes are frozen and
the module declares `gil_used = false` — though a free-threaded wheel is not yet shipped.

## License

MIT © the MDS authors.
