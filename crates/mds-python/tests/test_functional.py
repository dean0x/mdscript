"""Functional coverage of the seven public functions (AC-F*, AC-C1)."""

from __future__ import annotations

import pathlib
import sys

import pytest

import mdscript as m

# ── compile: markdown vs messages, vars, base_path (F1–F4) ──────────────────────


def test_f1_markdown_result_shape() -> None:
    r = m.compile("Hello World!\n")
    assert r.kind == "markdown"
    assert r.output == "Hello World!\n"
    assert r.messages is None
    assert r.warnings == []
    assert r.dependencies == []


def test_f2_messages_result_shape() -> None:
    r = m.compile("@message system:\nYou help.\n@end\n@message user:\nHi\n@end\n")
    assert r.kind == "messages"
    assert r.output is None  # inactive payload is None
    assert [msg.role for msg in (r.messages or [])] == ["system", "user"]
    assert (r.messages or [])[0].content.strip() == "You help."


def test_f3_runtime_vars_override_frontmatter() -> None:
    src = "---\nname: Alice\n---\nHello {name}!\n"
    assert "Alice" in m.compile(src).output  # type: ignore[operator]
    r = m.compile(src, vars={"name": "Override"})
    assert "Hello Override!" in (r.output or "")


@pytest.mark.skipif(
    sys.platform == "win32",
    reason="string-source base_path relative imports hit a core Windows path bug "
    "(#133 — canonicalize returns a \\\\?\\ verbatim path); compile_file / "
    "compile_virtual are unaffected. The binding forwards base_path correctly.",
)
def test_f4_base_path_import(fixtures: pathlib.Path) -> None:
    src = '@import { greet } from "./import_provider.mds"\n\n{greet("Test")}\n'
    r = m.compile(src, base_path=fixtures)
    assert "Hello Test!" in (r.output or "")


def test_f3_vars_empty_and_none_equivalent() -> None:
    assert m.compile("Hi\n", vars=None).output == m.compile("Hi\n").output
    assert m.compile("Hi\n", vars={}).output == "Hi\n"


# ── compile_file: str / PathLike, deps (F5) ─────────────────────────────────────


def test_f5_compile_file_str(fixtures: pathlib.Path) -> None:
    r = m.compile_file(str(fixtures / "simple.mds"))
    assert r.kind == "markdown"
    assert "Hello Alice!" in (r.output or "")
    assert "3 items" in (r.output or "")


def test_f5_compile_file_pathlike(fixtures: pathlib.Path) -> None:
    r = m.compile_file(fixtures / "simple.mds")  # PathLike
    assert "Hello Alice!" in (r.output or "")


def test_f5_compile_file_deps_absolute_entry_excluded(fixtures: pathlib.Path) -> None:
    r = m.compile_file(fixtures / "import_consumer.mds")
    assert "Hello World!" in (r.output or "")
    assert r.dependencies, "consumer imports a provider"
    assert all(pathlib.Path(d).is_absolute() for d in r.dependencies)
    # the entry file itself is excluded from dependencies
    assert not any(d.endswith("import_consumer.mds") for d in r.dependencies)
    assert any(d.endswith("import_provider.mds") for d in r.dependencies)


# ── check / check_file (F6, F7) ─────────────────────────────────────────────────


def test_f6_check_source() -> None:
    assert m.check("Hello {n}!\n", vars={"n": "x"}).warnings == []


def test_f7_check_file(fixtures: pathlib.Path) -> None:
    assert m.check_file(fixtures / "simple.mds").warnings == []
    assert m.check_file(str(fixtures / "var.mds"), vars={"name": "World"}).warnings == []


# ── compile_virtual / check_virtual (F8, F9) ────────────────────────────────────

VIRTUAL = {
    "main.mds": '@import { g } from "./lib.mds"\n{g("V")}\n',
    "lib.mds": "@define g(x):\nHi {x}!\n@end\n@export g\n",
}


def test_f8_compile_virtual_cross_module_entry_excluded() -> None:
    r = m.compile_virtual(VIRTUAL, "main.mds")
    assert r.kind == "markdown"
    assert "Hi V!" in (r.output or "")
    assert "main.mds" not in r.dependencies


def test_f9_check_virtual_mixed_content_ok() -> None:
    assert m.check_virtual(VIRTUAL, "main.mds").warnings == []


def test_f8_compile_virtual_messages() -> None:
    mods = {"m.mds": "@message user:\nHi {who}\n@end\n"}
    r = m.compile_virtual(mods, "m.mds", vars={"who": "there"})
    assert r.kind == "messages"
    assert (r.messages or [])[0].content.strip() == "Hi there"


# ── scan_imports (F10) ──────────────────────────────────────────────────────────


def test_f10_scan_imports_dedup_and_order() -> None:
    src = '@import "./a.mds"\n@import "./a.mds"\n@import { x } from "./b.mds"\n'
    assert m.scan_imports(src) == ["./a.mds", "./b.mds"]


def test_f10_scan_imports_frontmatter_first() -> None:
    src = (
        "---\nimports:\n  - path: ./fm.mds\n---\n"
        '@import "./body.mds"\n'
    )
    assert m.scan_imports(src) == ["./fm.mds", "./body.mds"]


def test_f10_scan_imports_empty_when_none() -> None:
    assert m.scan_imports("Just prose.\n") == []


def test_f10_scan_imports_is_positional_only() -> None:
    with pytest.raises(TypeError):
        m.scan_imports(source="Hi\n")  # type: ignore[call-arg]


# ── warnings surfaced, never printed (F11) ──────────────────────────────────────


def test_f11_warnings_surfaced_not_printed(
    capfd: pytest.CaptureFixture[str],
) -> None:
    # `@include` of a body-less module (only @define/@export) warns rather than
    # errors. The binding uses the *collecting* core APIs, so the warning is
    # returned in `.warnings` and never written to stderr. Uses compile_virtual so
    # this is cross-platform (no OS path resolution — see #133 for the Windows
    # base_path limitation).
    mods = {
        "main.mds": '@import "./lib.mds" as provider\n@include provider\n',
        "lib.mds": "@define g(x):\nHi {x}!\n@end\n@export g\n",
    }
    r = m.compile_virtual(mods, "main.mds")
    captured = capfd.readouterr()
    assert captured.err == "", "binding must not print warnings to stderr"
    assert any("empty output" in w for w in r.warnings), r.warnings
    assert r.kind == "markdown"


# ── C1: keyword-only / signature rules ──────────────────────────────────────────


def test_c1_vars_and_base_path_are_keyword_only() -> None:
    with pytest.raises(TypeError):
        m.compile("Hi\n", {"n": "x"})  # type: ignore[misc]


def test_c1_unknown_kwarg_raises_type_error() -> None:
    with pytest.raises(TypeError):
        m.compile("Hi\n", bogus=1)  # type: ignore[call-arg]
    with pytest.raises(TypeError):
        m.compile_file("x.mds", base_path="/tmp")  # type: ignore[call-arg]
