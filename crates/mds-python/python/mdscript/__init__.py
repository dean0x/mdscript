"""mdscript — composable LLM prompt template compiler (native Python bindings).

Compile ``.mds`` templates to Markdown or structured chat messages in-process, via
the same Rust core that powers the MDS CLI and Node.js/WASM bindings. Output is
byte-identical across all bindings.

Example
-------
>>> import mdscript
>>> r = mdscript.compile("Hello {name}!", vars={"name": "Alice"})
>>> r.kind, r.output
('markdown', 'Hello Alice!')

Errors raise :class:`MdsError`, which carries ``.code``, ``.message``, ``.help``,
and ``.span``. Compilation is synchronous CPU work and releases the GIL, so it
parallelises across threads; wrap a call in ``asyncio.to_thread`` for async code.
"""

from __future__ import annotations

from importlib import metadata as _metadata

from ._mdscript import (
    CheckResult,
    CompileResult,
    MdsError,
    Message,
    Span,
    check,
    check_file,
    check_virtual,
    compile,
    compile_file,
    compile_virtual,
    scan_imports,
)

# The native exception is registered under the extension submodule `_mdscript`.
# Retag it (and it alone — the result classes already declare `module = "mdscript"`)
# to the public package so `pickle`, `repr`, and tracebacks resolve `mdscript.MdsError`.
MdsError.__module__ = "mdscript"

try:
    __version__ = _metadata.version("mdscript")
except _metadata.PackageNotFoundError:  # pragma: no cover - source tree without an install
    __version__ = "0.0.0"

__all__ = [
    "CheckResult",
    "CompileResult",
    "MdsError",
    "Message",
    "Span",
    "__version__",
    "check",
    "check_file",
    "check_virtual",
    "compile",
    "compile_file",
    "compile_virtual",
    "scan_imports",
]
