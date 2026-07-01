# mdscript

Native **Python bindings** for [MDS (Markdown Script)](https://github.com/dean0x/mdscript) —
a composable LLM prompt-template compiler. Compile `.mds` templates to Markdown or
structured chat messages in-process, backed by the same Rust core as the MDS CLI and
the Node.js / WASM bindings. Output is byte-identical across every binding.

```bash
pip install mdscript
```

> Wheels ship as `cp311-abi3` (CPython 3.11+, one wheel per platform). Building from
> source needs a Rust toolchain and `python3` on `PATH`.

## Quick start

```python
import mdscript

# Markdown template
r = mdscript.compile("Hello {name}!", vars={"name": "Alice"})
assert r.kind == "markdown"
assert r.output == "Hello Alice!"

# @message template → structured messages
r = mdscript.compile("@message user:\nHi\n@end\n")
assert r.kind == "messages"
assert r.messages[0].role == "user"
assert r.output is None            # inactive payload is None

# Validate without rendering
mdscript.check("Hello {name}!", vars={"name": "Bob"})

# Compile a file (dependencies come back as absolute paths)
r = mdscript.compile_file("prompts/agent.mds")
print(r.dependencies)
```

## API

All compile/check functions return a typed, picklable result. Keyword arguments are
keyword-only; `scan_imports` takes its argument positionally.

| Function | Signature |
|----------|-----------|
| `compile` | `compile(source, *, vars=None, base_path=None) -> CompileResult` |
| `compile_file` | `compile_file(path, *, vars=None) -> CompileResult` |
| `compile_virtual` | `compile_virtual(modules, entry, *, vars=None) -> CompileResult` |
| `check` | `check(source, *, vars=None, base_path=None) -> CheckResult` |
| `check_file` | `check_file(path, *, vars=None) -> CheckResult` |
| `check_virtual` | `check_virtual(modules, entry, *, vars=None) -> CheckResult` |
| `scan_imports` | `scan_imports(source, /) -> list[str]` |

- `path` / `base_path` accept `str` or `os.PathLike`.
- `vars` is a mapping of string keys to JSON-compatible values; a non-mapping raises
  `MdsError(code="mds::invalid_options")`.
- `compile_virtual` / `check_virtual` resolve imports against an in-memory map;
  `entry` must be a key in `modules` (no source injection occurs).

### Result objects

`CompileResult` exposes `.kind` (`"markdown"` | `"messages"`), `.output` (`str | None`),
`.messages` (`list[Message] | None`), `.warnings`, and `.dependencies`. `CheckResult`
exposes `.warnings`. Both offer `.to_dict()` (the canonical discriminated-union dict,
inactive key absent) and `.to_json()`. Results are frozen, comparable by value,
intentionally unhashable, and picklable.

### Errors

Every failure raises `mdscript.MdsError` (a subclass of `Exception`):

```python
try:
    mdscript.compile("Hello {undefined}!")
except mdscript.MdsError as e:
    print(e.code)          # "mds::undefined_var"
    print(str(e))          # == e.message
    print(e.help)          # hint, or None
    if e.span:
        print(e.span.line, e.span.column)   # 1-indexed
```

## Concurrency

Compilation is synchronous, stateless CPU work and **releases the GIL**, so calls
parallelise across threads. For `asyncio`, offload with `asyncio.to_thread(mdscript.compile, src)`.
The extension is also free-threading (`cp314t`) ready — result classes are frozen and
the module declares `gil_used = false` — though a free-threaded wheel is not yet shipped.

## License

MIT © the MDS authors.
