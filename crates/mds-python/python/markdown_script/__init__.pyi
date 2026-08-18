"""Public type surface for the ``markdown_script`` package.

Everything is re-exported from the native ``._markdown_script`` extension; see
``_markdown_script.pyi`` for the full signatures.
"""

from __future__ import annotations

from ._markdown_script import CheckResult as CheckResult
from ._markdown_script import CompileResult as CompileResult
from ._markdown_script import LintDiagnostic as LintDiagnostic
from ._markdown_script import LintFileReport as LintFileReport
from ._markdown_script import LintResult as LintResult
from ._markdown_script import MdsError as MdsError
from ._markdown_script import Message as Message
from ._markdown_script import Span as Span
from ._markdown_script import check as check
from ._markdown_script import check_file as check_file
from ._markdown_script import check_virtual as check_virtual
from ._markdown_script import compile as compile
from ._markdown_script import compile_file as compile_file
from ._markdown_script import compile_virtual as compile_virtual
from ._markdown_script import lint as lint
from ._markdown_script import lint_file as lint_file
from ._markdown_script import lint_virtual as lint_virtual
from ._markdown_script import scan_imports as scan_imports

__version__: str
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
