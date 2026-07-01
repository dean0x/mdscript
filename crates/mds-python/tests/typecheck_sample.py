"""Type-checking sample — must pass `mypy --strict` and `pyright` cleanly (AC-C6).

Exercises the public API with correct types, including `Optional` narrowing on the
discriminated `CompileResult`.
"""

from __future__ import annotations

import pathlib

import mdscript
from mdscript import CheckResult, CompileResult, MdsError, Message, Span


def render_markdown() -> str:
    result: CompileResult = mdscript.compile("Hello {name}!", vars={"name": "Alice"})
    if result.output is not None:  # narrow str | None -> str
        return result.output
    return ""


def collect_roles() -> list[str]:
    result = mdscript.compile("@message user:\nHi\n@end\n")
    roles: list[str] = []
    if result.messages is not None:
        for message in result.messages:
            msg: Message = message
            roles.append(msg.role)
    return roles


def compile_from_file(path: pathlib.Path) -> CompileResult:
    return mdscript.compile_file(path, vars={"count": 3})


def compile_virtual_graph() -> CompileResult:
    modules: dict[str, str] = {"main.mds": "hi\n"}
    return mdscript.compile_virtual(modules, "main.mds")


def validate(source: str) -> list[str]:
    check_result: CheckResult = mdscript.check(source, base_path="/tmp")
    return check_result.warnings


def imports(source: str) -> list[str]:
    return mdscript.scan_imports(source)


def describe_error() -> str:
    try:
        mdscript.compile("{undef}")
    except MdsError as err:
        code: str = err.code
        span: Span | None = err.span
        if span is not None:
            line: int | None = span.line
            column: int | None = span.column
            _ = (line, column)
        return code
    return "ok"


def package_version() -> str:
    return mdscript.__version__
