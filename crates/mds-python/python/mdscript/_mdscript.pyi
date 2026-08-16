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

    ``source_map`` is a Source Map v3 ``dict`` when ``source_map=True`` was passed
    to the compile function and the result is Markdown; otherwise ``None``.
    The wire key in ``to_dict()`` / ``to_json()`` is ``"sourceMap"`` (camelCase).

    **``to_dict()`` vs ``to_json()`` asymmetry**: ``to_dict()`` always includes
    the ``"sourceMap"`` key (``None`` when no map was generated) for
    Python-idiomatic always-present attribute access. ``to_json()`` omits the key
    when absent — that is the canonical wire format shared with Node.js and WASM.
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
    @property
    def source_map(self) -> dict[str, Any] | None: ...
    def __new__(cls, canonical: Mapping[str, Any]) -> CompileResult: ...
    def to_dict(self) -> dict[str, Any]: ...
    def to_json(self) -> str: ...
    def __eq__(self, other: object, /) -> bool: ...
    __hash__: None  # type: ignore[assignment]

@final
class LintDiagnostic:
    """A single lint finding within a :class:`LintFileReport` (frozen, unhashable).

    Attributes map directly to the canonical wire-format diagnostic object.
    ``help`` is ``None`` when the rule emits no hint; ``span`` is ``None`` for
    rules that do not attach a source offset. Both attributes are always present.
    """

    @property
    def rule(self) -> str: ...
    @property
    def severity(self) -> str: ...
    @property
    def message(self) -> str: ...
    @property
    def help(self) -> str | None: ...
    @property
    def fixable(self) -> bool: ...
    @property
    def span(self) -> Span | None: ...
    @property
    def fix_edits(self) -> list[dict[str, Any]] | None: ...
    def __new__(
        cls,
        rule: str,
        severity: str,
        message: str,
        help: str | None = ...,
        fixable: bool = ...,
        span: Span | None = ...,
        fix_edits_json: str | None = ...,
    ) -> LintDiagnostic: ...
    def to_dict(self) -> dict[str, Any]: ...
    def to_json(self) -> str: ...
    def __eq__(self, other: object, /) -> bool: ...
    __hash__: None  # type: ignore[assignment]

@final
class LintFileReport:
    """Per-file findings group from :attr:`LintResult.files` (frozen, unhashable).

    ``file`` is the path key for this file's diagnostics; ``diagnostics`` is a
    typed list of :class:`LintDiagnostic` objects with fully-typed attributes.
    """

    @property
    def file(self) -> str: ...
    @property
    def diagnostics(self) -> list[LintDiagnostic]: ...
    def __new__(
        cls,
        file: str,
        diagnostics: list[LintDiagnostic],
    ) -> LintFileReport: ...
    def to_dict(self) -> dict[str, Any]: ...
    def to_json(self) -> str: ...
    def __eq__(self, other: object, /) -> bool: ...
    __hash__: None  # type: ignore[assignment]

@final
class LintResult:
    """The result of :func:`lint`, :func:`lint_file`, or :func:`lint_virtual`.

    Core JSON shape: ``{"files":[...],"truncated":false,"version":1}``.
    When non-fatal warnings occur (e.g. unknown rule names),
    ``"lint_warnings"`` also appears in alphabetical key order between
    ``"files"`` and ``"truncated"``. Keys are in BTreeMap (alphabetical)
    order. The CLI surface writes warnings to stderr rather than including
    them in its JSON stdout.

    ``files`` is a list of typed :class:`LintFileReport` objects. Each report
    exposes ``.file`` (str) and ``.diagnostics`` (list[:class:`LintDiagnostic`]).
    """

    @property
    def version(self) -> int: ...
    @property
    def truncated(self) -> bool: ...
    @property
    def files(self) -> list[LintFileReport]: ...
    @property
    def lint_warnings(self) -> list[str]:
        """Non-fatal warnings raised while configuring the lint run.

        An unknown rule name in the ``rules`` mapping produces one entry here:
        the rule is not enforced (it does not exist) but the call still
        succeeds. Empty in the common case.
        """
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
    source_map: bool = ...,
    sources_content: bool = ...,
) -> CompileResult: ...
def compile_file(
    path: _StrPath,
    *,
    vars: _Vars | None = ...,
    source_map: bool = ...,
    sources_content: bool = ...,
) -> CompileResult: ...
def compile_virtual(
    modules: Mapping[str, str],
    entry: str,
    *,
    vars: _Vars | None = ...,
    source_map: bool = ...,
    sources_content: bool = ...,
) -> CompileResult: ...
def check(
    source: str,
    *,
    vars: _Vars | None = ...,
    base_path: _StrPath | None = ...,
    source_map: None = ...,
    sources_content: None = ...,
) -> CheckResult:
    """Validate without rendering.

    ``source_map`` and ``sources_content`` are present in the signature so that
    passing them raises ``MdsError(code="mds::invalid_options")`` rather than a
    bare ``TypeError``. Callers should never pass these — use :func:`compile` for
    source-map generation.
    """
    ...
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
    vars: _Vars | None = ...,
    base_path: _StrPath | None = ...,
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
