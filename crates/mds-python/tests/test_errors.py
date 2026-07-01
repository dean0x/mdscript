"""Error semantics: type, structured fields, codes, spans (AC-E*, AC-V2)."""

from __future__ import annotations

import pathlib

import pytest

import mdscript as m

# ── E1/E2: MdsError type + structured fields ────────────────────────────────────


def test_e1_mdserror_is_exception_and_catchable() -> None:
    assert issubclass(m.MdsError, Exception)
    with pytest.raises(m.MdsError):
        m.compile("Hello {undef}!\n")
    # also catchable as a plain Exception
    try:
        m.compile("Hello {undef}!\n")
    except Exception as e:  # noqa: BLE001
        assert isinstance(e, m.MdsError)


def test_e2_structured_fields_and_str_equals_message() -> None:
    try:
        m.compile("Hello {undef}!\n")
    except m.MdsError as e:
        assert isinstance(e.code, str) and e.code == "mds::undefined_var"
        assert isinstance(e.message, str) and e.message
        assert str(e) == e.message
        assert isinstance(e.help, str) and e.help  # undefined_var carries help
        assert e.span is not None
    else:
        pytest.fail("expected MdsError")


def test_e2_help_is_none_when_absent() -> None:
    # A syntax error carries no diagnostic help. (Not every error carries a span;
    # this one does not — span presence is asserted for undefined_var in E5.)
    try:
        m.compile("@import\n")
    except m.MdsError as e:
        assert e.code == "mds::syntax"
        assert e.help is None


# ── E3: each reachable core code (parametrized) ─────────────────────────────────

CORE_CASES = [
    ("mds::syntax", "@import\n"),
    ("mds::undefined_var", "Hello {undef}!\n"),
    ("mds::undefined_fn", "{nofn()}\n"),
    ("mds::arity", "@define f(x):\n{x}\n@end\n{f()}\n"),
    ("mds::type_error", "---\nn: 5\n---\n@for x in n:\n{x}\n@end\n"),
    ("mds::mixed_content", "Prose.\n\n@message user:\nHi\n@end\n"),
]


@pytest.mark.parametrize("code,src", CORE_CASES, ids=[c for c, _ in CORE_CASES])
def test_e3_core_error_codes(code: str, src: str) -> None:
    with pytest.raises(m.MdsError) as ei:
        m.compile(src)
    assert ei.value.code == code, ei.value.message


def test_e3_file_not_found() -> None:
    with pytest.raises(m.MdsError) as ei:
        m.compile_file("/no/such/mds/file.mds")
    assert ei.value.code == "mds::file_not_found"


def test_e3_circular_import_virtual() -> None:
    mods = {"a.mds": '@import "./b.mds"\n', "b.mds": '@import "./a.mds"\n'}
    with pytest.raises(m.MdsError) as ei:
        m.compile_virtual(mods, "a.mds")
    assert ei.value.code == "mds::circular_import"


def test_e3_mixed_content_on_check(fixtures: pathlib.Path) -> None:
    with pytest.raises(m.MdsError) as ei:
        m.check_file(fixtures / "mixed.mds")
    assert ei.value.code == "mds::mixed_content"


# ── E4: boundary codes, no path leak ────────────────────────────────────────────


def test_e4_invalid_options_code() -> None:
    with pytest.raises(m.MdsError) as ei:
        m.compile("Hi\n", vars="not-a-mapping")  # type: ignore[arg-type]
    assert ei.value.code == "mds::invalid_options"
    assert ei.value.span is None


def test_e4_resource_limit_code() -> None:
    with pytest.raises(m.MdsError) as ei:
        m.compile("x" * (10 * 1024 * 1024 + 1))
    assert ei.value.code == "mds::resource_limit"
    assert ei.value.span is None


def test_e4_error_messages_do_not_leak_rust_source_paths() -> None:
    # Boundary/compile errors must not contain internal Rust source file paths.
    for src in ("Hello {undef}!\n", "@import\n"):
        try:
            m.compile(src)
        except m.MdsError as e:
            assert ".rs" not in e.message
            assert "src/" not in e.message


# ── V2: non-mapping vars → invalid_options ──────────────────────────────────────


@pytest.mark.parametrize("bad", ["str", ["a", "b"], 42, 3.5, True])
def test_v2_non_mapping_vars(bad: object) -> None:
    with pytest.raises(m.MdsError) as ei:
        m.compile("Hi\n", vars=bad)  # type: ignore[arg-type]
    assert ei.value.code == "mds::invalid_options"


# ── E5: span byte offset + 1-indexed char line/column, None when absent ─────────


def test_e5_span_offset_and_line_column_single_line() -> None:
    src = "X" * 100 + "{undef}!\n"
    try:
        m.compile(src)
    except m.MdsError as e:
        assert e.span is not None
        # span points at the interpolation, deep in the source (byte offset tracked)
        assert e.span.offset == src.index("{undef") == 100
        assert e.span.length > 0
        assert e.span.line == 1
        # 1-indexed character column (ASCII → char offset == byte offset)
        assert e.span.column == e.span.offset + 1
        assert isinstance(e.span.offset, int)  # Python int — no truncation


def test_e5_span_line_increments_on_multiline() -> None:
    src = "line one\nline two {undef}\n"
    try:
        m.compile(src)
    except m.MdsError as e:
        assert e.span is not None
        assert e.span.line == 2
        assert e.span.column and e.span.column > 1


def test_e5_span_none_when_core_reports_none() -> None:
    # A synthesized boundary error carries no span.
    try:
        m.compile("x" * (10 * 1024 * 1024 + 1))
    except m.MdsError as e:
        assert e.span is None
