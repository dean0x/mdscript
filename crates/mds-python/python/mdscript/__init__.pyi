"""Public type surface for the ``mdscript`` package.

Everything is re-exported from the native ``._mdscript`` extension; see
``_mdscript.pyi`` for the full signatures.
"""

from __future__ import annotations

from ._mdscript import CheckResult as CheckResult
from ._mdscript import CompileResult as CompileResult
from ._mdscript import MdsError as MdsError
from ._mdscript import Message as Message
from ._mdscript import Span as Span
from ._mdscript import check as check
from ._mdscript import check_file as check_file
from ._mdscript import check_virtual as check_virtual
from ._mdscript import compile as compile
from ._mdscript import compile_file as compile_file
from ._mdscript import compile_virtual as compile_virtual
from ._mdscript import scan_imports as scan_imports

__version__: str
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
