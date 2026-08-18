"""markdown_script — composable LLM prompt template compiler (native Python bindings).

Compile ``.mds`` templates to Markdown or structured chat messages in-process, via
the same Rust core that powers the MDS CLI and Node.js/WASM bindings. Output is
byte-identical across all bindings.

Example
-------
>>> import markdown_script
>>> r = markdown_script.compile("Hello {{name}}!", vars={"name": "Alice"})
>>> r.kind, r.output
('markdown', 'Hello Alice!')

Errors raise :class:`MdsError`, which carries ``.code``, ``.message``, ``.help``,
and ``.span``. Compilation is synchronous CPU work and releases the GIL, so it
parallelises across threads; wrap a call in ``asyncio.to_thread`` for async code.
"""

from __future__ import annotations

from importlib import metadata as _metadata

from ._markdown_script import (
    CheckResult,
    CompileResult,
    LintDiagnostic,
    LintFileReport,
    LintResult,
    MdsError,
    Message,
    Span,
    check,
    check_file,
    check_virtual,
    compile,
    compile_file,
    compile_virtual,
    lint,
    lint_file,
    lint_virtual,
    scan_imports,
)

# The native exception is registered under the extension submodule `_markdown_script`.
# Retag it (and it alone — the result classes already declare `module = "markdown_script"`)
# to the public package so `pickle`, `repr`, and tracebacks resolve `markdown_script.MdsError`.
MdsError.__module__ = "markdown_script"

try:
    __version__ = _metadata.version("markdown-script")
except _metadata.PackageNotFoundError:  # pragma: no cover - source tree without an install
    __version__ = "0.0.0"

__all__ = [
    "CheckResult",
    "CompileResult",
    "LintDiagnostic",
    "LintFileReport",
    "LintResult",
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
    "lint",
    "lint_file",
    "lint_virtual",
    "scan_imports",
]
