"""Type stubs for the native ``mdscript._mdscript`` extension module.

The runtime objects are implemented in Rust (PyO3). These stubs describe the public
surface for ``mypy``/``pyright``. Result classes are frozen — their attributes are
read-only properties, so they are declared with ``@property``.
"""

from __future__ import annotations

from collections.abc import Mapping
from os import PathLike
from typing import Any, Literal, final

# `str | os.PathLike[str]` — accepted for `path` and `base_path`.
_StrPath = str | PathLike[str]
# A `vars` mapping: string keys to JSON-compatible values.
_Vars = Mapping[str, Any]

@final
class Span:
    """A source span attached to an :class:`MdsError` (frozen, unhashable)."""

    @property
    def offset(self) -> int: ...
    @property
    def length(self) -> int: ...
    @property
    def line(self) -> int | None: ...
    @property
    def column(self) -> int | None: ...
    def __new__(
        cls,
        offset: int,
        length: int,
        line: int | None = ...,
        column: int | None = ...,
    ) -> Span: ...
    def to_dict(self) -> dict[str, Any]: ...
    def to_json(self) -> str: ...
    def __eq__(self, other: object, /) -> bool: ...
    __hash__: None  # type: ignore[assignment]

@final
class Message:
    """A single chat message from a `@message`-bearing template (frozen)."""

    @property
    def role(self) -> str: ...
    @property
    def content(self) -> str: ...
    def __new__(cls, role: str, content: str) -> Message: ...
    def to_dict(self) -> dict[str, str]: ...
    def to_json(self) -> str: ...
    def __eq__(self, other: object, /) -> bool: ...
    __hash__: None  # type: ignore[assignment]

@final
class CheckResult:
    """The result of :func:`check`, :func:`check_file`, or :func:`check_virtual`."""

    @property
    def warnings(self) -> list[str]: ...
    def __new__(cls, warnings: list[str]) -> CheckResult: ...
    def to_dict(self) -> dict[str, Any]: ...
    def to_json(self) -> str: ...
    def __eq__(self, other: object, /) -> bool: ...
    __hash__: None  # type: ignore[assignment]

@final
class CompileResult:
    """The result of :func:`compile`, :func:`compile_file`, or :func:`compile_virtual`.

    ``kind`` is ``"markdown"`` or ``"messages"``. On a ``markdown`` result
    ``messages`` is ``None``; on a ``messages`` result ``output`` is ``None``.
    """

    @property
    def kind(self) -> Literal["markdown", "messages"]: ...
    @property
    def output(self) -> str | None: ...
    @property
    def messages(self) -> list[Message] | None: ...
    @property
    def warnings(self) -> list[str]: ...
    @property
    def dependencies(self) -> list[str]: ...
    def __new__(cls, canonical: Mapping[str, Any]) -> CompileResult: ...
    def to_dict(self) -> dict[str, Any]: ...
    def to_json(self) -> str: ...
    def __eq__(self, other: object, /) -> bool: ...
    __hash__: None  # type: ignore[assignment]

@final
class LintResult:
    """The result of :func:`lint`, :func:`lint_file`, or :func:`lint_virtual`.

    Canonical JSON shape: ``{"files": [...], "truncated": false, "version": 1}``.
    Keys are in BTreeMap (alphabetical) order — byte-identical across all surfaces.
    """

    @property
    def version(self) -> int: ...
    @property
    def truncated(self) -> bool: ...
    @property
    def files(self) -> list[dict[str, Any]]: ...
    def __new__(cls, canonical: Mapping[str, Any]) -> LintResult: ...
    def to_dict(self) -> dict[str, Any]: ...
    def to_json(self) -> str: ...
    def __eq__(self, other: object, /) -> bool: ...
    __hash__: None  # type: ignore[assignment]

class MdsError(Exception):
    """Raised for every MDS compilation failure.

    ``str(err) == err.message``.
    """

    code: str
    message: str
    help: str | None
    span: Span | None

def compile(
    source: str,
    *,
    vars: _Vars | None = ...,
    base_path: _StrPath | None = ...,
) -> CompileResult: ...
def compile_file(path: _StrPath, *, vars: _Vars | None = ...) -> CompileResult: ...
def compile_virtual(
    modules: Mapping[str, str],
    entry: str,
    *,
    vars: _Vars | None = ...,
) -> CompileResult: ...
def check(
    source: str,
    *,
    vars: _Vars | None = ...,
    base_path: _StrPath | None = ...,
) -> CheckResult: ...
def check_file(path: _StrPath, *, vars: _Vars | None = ...) -> CheckResult: ...
def check_virtual(
    modules: Mapping[str, str],
    entry: str,
    *,
    vars: _Vars | None = ...,
) -> CheckResult: ...
def scan_imports(source: str, /) -> list[str]: ...
def lint(
    source: str,
    *,
    base_path: _StrPath | None = ...,
    vars: _Vars | None = ...,
    rules: Mapping[str, str] | None = ...,
) -> LintResult: ...
def lint_file(
    path: _StrPath,
    *,
    vars: _Vars | None = ...,
    rules: Mapping[str, str] | None = ...,
) -> LintResult: ...
def lint_virtual(
    modules: Mapping[str, str],
    entry: str,
    *,
    vars: _Vars | None = ...,
    rules: Mapping[str, str] | None = ...,
) -> LintResult: ...
