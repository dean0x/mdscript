"""Type-checking sample — must pass `mypy --strict` and `pyright` cleanly (AC-C6).

Exercises the public API with correct types, including `Optional` narrowing on the
discriminated `CompileResult` and explicit coverage of the lint surface (AC-C6 per
review #171: lint / lint_file / lint_virtual and LintResult attributes).
"""

from __future__ import annotations

import pathlib
from typing import Any

import mdscript
from mdscript import (
    CheckResult,
    CompileResult,
    LintDiagnostic,
    LintFileReport,
    LintResult,
    MdsError,
    Message,
    Span,
)


def render_markdown() -> str:
    result: CompileResult = mdscript.compile("Hello {{name}}!", vars={"name": "Alice"})
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
        mdscript.compile("{{undef}}")
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


# ---------------------------------------------------------------------------
# Lint surface (AC-C6 coverage — review #171)
# All three lint entry-points and every LintResult attribute are exercised with
# explicitly-annotated locals so a stub type error surfaces under strict checkers.
# ---------------------------------------------------------------------------


def lint_source(source: str) -> int:
    """Lint inline source; return the number of files in the result."""
    result: LintResult = mdscript.lint(source)
    version: int = result.version
    truncated: bool = result.truncated
    files: list[LintFileReport] = result.files
    _ = (version, truncated)
    return len(files)


def lint_typed_access(source: str) -> list[str]:
    """Demonstrate fully-typed attribute access on lint results (B6/F10)."""
    result: LintResult = mdscript.lint(source)
    # AC-224-1 / D8: the unknown-rule warning channel is typed as list[str], so a
    # consumer can read it without a cast or a `type: ignore`.
    lint_warnings: list[str] = result.lint_warnings
    _ = lint_warnings
    rules: list[str] = []
    report: LintFileReport
    for report in result.files:
        file_key: str = report.file
        diag: LintDiagnostic
        for diag in report.diagnostics:
            rule: str = diag.rule
            severity: str = diag.severity
            message: str = diag.message
            help_text: str | None = diag.help
            fixable: bool = diag.fixable
            span: Span | None = diag.span
            _ = (file_key, severity, message, help_text, fixable, span)
            rules.append(rule)
    return rules


def lint_source_with_options(source: str) -> str:
    """Lint source with all optional keyword arguments; return canonical JSON."""
    rules: dict[str, str] = {"shadow-variable": "warn", "unused-variable": "off"}
    result: LintResult = mdscript.lint(
        source,
        base_path="/tmp",
        vars={"env": "ci"},
        rules=rules,
    )
    d: dict[str, Any] = result.to_dict()
    _ = d
    return result.to_json()


def lint_from_file(path: pathlib.Path) -> LintResult:
    """Lint a .mds file on disk and return the result."""
    return mdscript.lint_file(
        path,
        vars={"count": 1},
        rules={"unused-variable": "off"},
    )


def lint_virtual_graph() -> bool:
    """Lint a virtual module graph; return whether the result was truncated."""
    modules: dict[str, str] = {"entry.mds": "Hello!\n"}
    result: LintResult = mdscript.lint_virtual(
        modules,
        "entry.mds",
        vars={"name": "world"},
        rules={"shadow-variable": "warn"},
    )
    files: list[LintFileReport] = result.files
    j: str = result.to_json()
    _ = (files, j)
    return result.truncated
