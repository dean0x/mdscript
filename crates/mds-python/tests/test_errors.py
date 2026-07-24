"""Error semantics: type, structured fields, codes, spans (AC-E*, AC-V2)."""

from __future__ import annotations

import pathlib

import pytest

import mdscript as m

# ── E1/E2: MdsError type + structured fields ────────────────────────────────────


def test_e1_mdserror_is_exception_and_catchable() -> None:
    assert issubclass(m.MdsError, Exception)
    with pytest.raises(m.MdsError):
        m.compile("Hello {{undef}}!\n")
    # also catchable as a plain Exception
    try:
        m.compile("Hello {{undef}}!\n")
    except Exception as e:  # noqa: BLE001
        assert isinstance(e, m.MdsError)


def test_e2_structured_fields_and_str_equals_message() -> None:
    try:
        m.compile("Hello {{undef}}!\n")
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
    ("mds::undefined_var", "Hello {{undef}}!\n"),
    ("mds::undefined_fn", "{{nofn()}}\n"),
    ("mds::arity", "@define f(x):\n{{x}}\n@end\n{{f()}}\n"),
    ("mds::type_error", "---\nn: 5\n---\n@for x in n:\n{{x}}\n@end\n"),
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


def test_e3_mixed_content_on_check_virtual() -> None:
    # check_virtual routes through the same intrinsic dispatch as check_file (per
    # FEATURE_KNOWLEDGE: "check_* now routes through intrinsic dispatch: rejects
    # mixed content") — same source used in test_e3_core_error_codes' mds::mixed_content
    # case, just via the virtual-FS entrypoint instead of a plain string.
    mods = {"a.mds": "Prose.\n\n@message user:\nHi\n@end\n"}
    with pytest.raises(m.MdsError) as ei:
        m.check_virtual(mods, "a.mds")
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
    for src in ("Hello {{undef}}!\n", "@import\n"):
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
    src = "X" * 100 + "{{undef}}!\n"
    try:
        m.compile(src)
    except m.MdsError as e:
        assert e.span is not None
        # span points at the interpolation, deep in the source (byte offset tracked)
        assert e.span.offset == src.index("{{undef") == 100
        assert e.span.length > 0
        assert e.span.line == 1
        # 1-indexed character column (ASCII → char offset == byte offset)
        assert e.span.column == e.span.offset + 1
        assert isinstance(e.span.offset, int)  # Python int — no truncation
    else:
        pytest.fail("expected MdsError")


def test_e5_span_line_increments_on_multiline() -> None:
    src = "line one\nline two {{undef}}\n"
    try:
        m.compile(src)
    except m.MdsError as e:
        assert e.span is not None
        assert e.span.line == 2
        assert e.span.column and e.span.column > 1
    else:
        pytest.fail("expected MdsError")


def test_e5_span_none_when_core_reports_none() -> None:
    # A synthesized boundary error carries no span.
    try:
        m.compile("x" * (10 * 1024 * 1024 + 1))
    except m.MdsError as e:
        assert e.span is None
    else:
        pytest.fail("expected MdsError")


# ── T-14: ESC-injection hardening — Python surface (issue #176 / CWE-150) ──────
#
# Two sub-tests:
#  E11: error path — @include alias with U+001B in mid-token; err.message must
#       carry the sanitized \\u001B literal; str(e) == e.message (AC-C2) must hold.
#  E12: lint path — frontmatter key with U+001B; LintDiagnostic.message clean.
#
# Naming: test_e11_* / test_e12_* — chosen so -k substrings cannot collide with
# other tests (PF-008: pytest -k matches substrings of the full node id).


def test_e11_control_chars_in_message_are_escaped() -> None:
    """T-14 / E11 [AC-F3, AC-C2]: error-path sanitization for Python surface.

    @include with a raw ESC byte (U+001B) mid-alias is rejected by the parser
    with MdsError::Syntax("invalid include alias: 'fo<ESC>o'"). After Change #1,
    serialize() sanitizes the message so e.message contains no raw control bytes
    and the sanitized \\u001B literal is visible. AC-C2 (str(e) == e.message) must
    still hold because both use the same sanitized string.
    """
    esc = "\x1b"
    source = f"@include fo{esc}o\n"
    try:
        m.compile(source)
    except m.MdsError as e:
        msg = e.message
        # No raw C0 (excl. \\t \\n), DEL, or C1 bytes.
        for i, ch in enumerate(msg):
            code = ord(ch)
            is_c0 = code < 0x20 and code not in (0x09, 0x0A)
            is_del = code == 0x7F
            is_c1 = 0x80 <= code <= 0x9F
            assert not (is_c0 or is_del or is_c1), (
                f"raw control char U+{code:04X} at index {i} must not appear in "
                f"e.message; got: {msg!r}"
            )
        # Sanitized literal must be present.
        assert "\\u001B" in msg, (
            f"sanitized \\u001B literal must appear in e.message; got: {msg!r}"
        )
        # AC-C2: str(e) == e.message.
        assert str(e) == e.message, (
            f"AC-C2 violated: str(e)={str(e)!r} != e.message={e.message!r}"
        )
    else:
        pytest.fail("expected m.MdsError to be raised")


def _assert_no_control_chars(s: str, label: str) -> None:
    """Assert no raw C0 (excl. \\t \\n), DEL, or C1 bytes appear in `s`."""
    for i, ch in enumerate(s):
        code = ord(ch)
        is_c0 = code < 0x20 and code not in (0x09, 0x0A)
        is_del = code == 0x7F
        is_c1 = 0x80 <= code <= 0x9F
        assert not (is_c0 or is_del or is_c1), (
            f"raw control char U+{code:04X} at index {i} must not appear in {label}; got: {s!r}"
        )


def test_e12_lint_virtual_esc_in_import_path_message_sanitized() -> None:
    """T-14 / E12 [AC-F4]: Python typed LintDiagnostic.message and as_json() sanitization.

    Uses the lint_virtual API with a module whose NAME contains a raw ESC byte (U+001B)
    to trigger a duplicate-import rule whose message embeds the raw path — a reachable
    end-to-end vector that exercises the Python typed surface without touching YAML parsing.

    Verifies:
    (a) LintDiagnostic.message contains no raw C0/DEL/C1 bytes (typed attribute clean)
    (b) LintDiagnostic.message contains the sanitized \\u001B literal (explicit evidence)
    (c) LintDiagnostic.to_dict()["message"] is identical to .message (parity guard, PF-007)
    """
    esc = "\x1b"
    # Module whose name contains a raw ESC byte — import path embeds it in the message.
    module_name = f"fo{esc}o.mds"
    modules = {
        module_name: "hi\n",
        # Import the same module twice to trigger duplicate-import; message will embed module_name.
        "main.mds": f'@import "./{module_name}"\n@import "./{module_name}"\n',
    }
    result = m.lint_virtual(modules, "main.mds")

    files = result.files
    assert files, "expected at least one LintFileReport from lint_virtual"
    all_diags = [d for fr in files for d in fr.diagnostics]
    assert all_diags, (
        "expected at least one LintDiagnostic (duplicate-import should fire for "
        "the twice-imported module)"
    )

    for diag in all_diags:
        msg = diag.message
        assert isinstance(msg, str) and msg, "message must be a non-empty string"

        # (a) No raw C0/DEL/C1 bytes in typed .message attribute.
        _assert_no_control_chars(msg, "LintDiagnostic.message")

        # (b) At least one diagnostic must carry the sanitized \\u001B literal —
        #     confirming the ESC byte in the module name was sanitized, not dropped.
        # (Only the duplicate-import diagnostic embeds the path; check all.)

    esc_in_messages = [d for d in all_diags if "\\u001B" in d.message or "\\u001b" in d.message]
    assert esc_in_messages, (
        "expected at least one diagnostic whose message carries the sanitized "
        "\\u001B literal (module path); got: "
        + str([d.message for d in all_diags])
    )

    # (c) Parity guard: to_dict()["message"] must equal .message (PF-007).
    # as_json() / to_dict() must not re-introduce raw control bytes from pyclass fields.
    for diag in all_diags:
        d_dict = diag.to_dict()
        assert isinstance(d_dict, dict), "to_dict() must return a dict"
        dict_msg = d_dict.get("message", "")
        assert isinstance(dict_msg, str), "to_dict()[message] must be a string"
        assert dict_msg == diag.message, (
            f"to_dict()[message] must equal .message; "
            f"typed={diag.message!r}, dict={dict_msg!r}"
        )


# ── D2: type_mismatch_at — span present on @if cross-type comparison ─────────────


def test_d2_type_mismatch_span_is_not_none() -> None:
    """D2: cross-type == in @if now carries a source span via type_mismatch_at."""
    src = "---\nx: 3\n---\n@if x == \"3\":\nyes\n@end\n"
    try:
        m.compile(src)
    except m.MdsError as e:
        assert e.code == "mds::type_mismatch", f"expected type_mismatch, got: {e.code}"
        assert e.span is not None, (
            "D2: type_mismatch from @if must carry a span (type_mismatch_at)"
        )
        assert isinstance(e.span.offset, int), "span.offset must be an int"
        assert e.span.length > 0, "span.length must be > 0"
        # line/column must be populated (the error points at the @if line).
        assert e.span.line is not None, "span.line must be present for @if type_mismatch"
        assert e.span.column is not None, "span.column must be present for @if type_mismatch"
    else:
        pytest.fail("expected MdsError")
