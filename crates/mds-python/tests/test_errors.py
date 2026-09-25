"""Error semantics: type, structured fields, codes, spans (AC-E*, AC-V2)."""

from __future__ import annotations

import pathlib

import pytest

import markdown_script as m

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


def _assert_no_control_chars(s: str, label: str) -> None:
    """Assert no raw C0 (excl. \\t \\n), DEL, C1, bidi, separator, or BOM char in `s`.

    `\\n` is permitted because this helper also runs against HUMAN-mode output;
    wire-mode newline escaping is asserted explicitly by the E13 test.
    """
    for i, ch in enumerate(s):
        code = ord(ch)
        is_c0 = code < 0x20 and code not in (0x09, 0x0A)
        is_del = code == 0x7F
        is_c1 = 0x80 <= code <= 0x9F
        # Bidi controls (Trojan Source, CVE-2021-42574), JS line/paragraph
        # separators, and the invisible BOM.
        is_format_hazard = (
            code in (0x200E, 0x200F, 0x2028, 0x2029, 0xFEFF)
            or 0x202A <= code <= 0x202E
            or 0x2066 <= code <= 0x2069
        )
        assert not (is_c0 or is_del or is_c1 or is_format_hazard), (
            f"raw hostile char U+{code:04X} at index {i} must not appear in {label}; got: {s!r}"
        )


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
        _assert_no_control_chars(msg, "e.message")
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


@pytest.mark.parametrize(
    "codepoint",
    # ESC (C0), DEL, NEL (C1), and the widened display-hazard class (#176): RLO
    # (Trojan Source, CVE-2021-42574), LRI (bidi isolate), LINE SEPARATOR (ends a JS
    # string literal), BOM (invisible). Numeric, never raw characters -- a literal
    # RLO would reverse how this source file displays.
    [0x1B, 0x7F, 0x85, 0x202E, 0x2066, 0x2028, 0xFEFF],
    ids=["ESC", "DEL", "NEL", "RLO", "LRI", "LS", "BOM"],
)
def test_e12_lint_virtual_ctrl_in_import_path_is_refused(codepoint: int) -> None:
    """T-14 / E12 [AC-F4]: a hostile import path is refused with an escaped error (#265).

    lint_virtual with an import naming a module whose NAME contains a raw control or
    format codepoint. Before #265 the name reached the duplicate-import rule, whose
    message embedded it; the import string is now refused at the input boundary, so
    the error itself must name the codepoint and show it escaped. The lint-message
    coverage this vector used to give lives on in the route-B sibling below.

    Verifies (``pytest.raises`` fails when nothing is raised, so every assertion
    below is reached -- PF-013):
    (a) the error code is ``mds::import``;
    (b) the message is exactly the forbidden-character refusal, naming ``U+XXXX`` and
        showing the codepoint as its six-character escape;
    (c) the message carries no raw hostile codepoint, and ``str(e) == e.message``.
    """
    module_name = f"fo{chr(codepoint)}o.mds"
    modules = {
        module_name: "hi\n",
        "main.mds": f'@import "./{module_name}"\n@import "./{module_name}"\n',
    }
    with pytest.raises(m.MdsError) as excinfo:
        m.lint_virtual(modules, "main.mds")
    e = excinfo.value

    # (a)
    assert e.code == "mds::import", e.message
    # (b) The escape text is built, never written literally (PF-018).
    escaped = "\\u" + f"{codepoint:04X}"
    assert e.message == (
        "import error: import path contains forbidden character "
        f'U+{codepoint:04X}: "./fo{escaped}o.mds"'
    )
    # (c)
    _assert_no_control_chars(e.message, "e.message")
    assert str(e) == e.message


@pytest.mark.parametrize(
    "ctrl_char,expected_escape,yaml_escape",
    [
        ("\x1b", "\\u001B", "\\x1B"),  # ESC (U+001B) — C0 control char
        ("\x7f", "\\u007F", "\\x7F"),  # DEL (U+007F)
        ("\x85", "\\u0085", "\\x85"),  # NEL (C1) — passes serde_yaml_ng
        (chr(0x202E), "\\u202E", "\\u202E"),  # RLO - Trojan Source display reversal
        (chr(0x2066), "\\u2066", "\\u2066"),  # LRI - bidi isolate
        (chr(0x2028), "\\u2028", "\\u2028"),  # LINE SEPARATOR
        (chr(0xFEFF), "\\uFEFF", "\\uFEFF"),  # BOM / ZWNBSP
    ],
    ids=["ESC", "DEL", "NEL", "RLO", "LRI", "LS", "BOM"],
)
def test_e12_route_b_lint_virtual_ctrl_in_frontmatter_key_message_sanitized(
    ctrl_char: str, expected_escape: str, yaml_escape: str
) -> None:
    """Route B sibling of test_e12_lint_virtual_ctrl_in_import_path_is_refused.

    Same control-char set, but carried by an UNUSED FRONTMATTER KEY (unused-variable)
    rather than an import path / module name (duplicate-import). #265 rejects
    hostile paths/module names at the input boundary, which retired route-A coverage
    for these characters -- a frontmatter key is not a path, so this route stays
    reachable and keeps message-escaping coverage alive.

    Written as a YAML double-quoted key with a YAML escape (e.g. ``\\x1B``) so the
    .mds source text itself carries no raw control byte (PF-018); serde_yaml_ng
    decodes the escape into the real control codepoint, which unused-variable embeds
    verbatim in its message before WIRE-sanitization escapes it back out.
    """
    source = f'---\n"a{yaml_escape}payload{yaml_escape}b": 1\n---\nHello\n'
    # Route-B contract check: the constructed .mds source carries no raw control
    # byte -- only the YAML escape text.
    assert ctrl_char not in source, (
        f"route B: source must carry no raw {ctrl_char!r} byte, only the YAML escape; "
        f"got: {source!r}"
    )

    result = m.lint_virtual({"main.mds": source}, "main.mds")
    all_diags = [d for fr in result.files for d in fr.diagnostics]
    assert any(d.rule == "unused-variable" for d in all_diags), (
        "route B: expected unused-variable to fire; got rules: "
        + str([d.rule for d in all_diags])
    )

    # (a) No raw control bytes in typed .message attribute.
    for diag in all_diags:
        msg = diag.message
        assert isinstance(msg, str) and msg, "message must be a non-empty string"
        _assert_no_control_chars(msg, "LintDiagnostic.message (route B)")

    # (b) Positive control (PF-013): the sanitized literal IS present -- proof the
    # raw control codepoint really reached the parsed frontmatter key before the
    # WIRE sanitizer escaped it back out.
    found_escaped = [d for d in all_diags if expected_escape in d.message]
    assert found_escaped, (
        f"route B: expected at least one diagnostic whose message carries the "
        f"sanitized {expected_escape!r} literal (frontmatter key); got: "
        + str([d.message for d in all_diags])
    )

    # Escaped, not stripped -- the payload text around it survives verbatim.
    assert any("payload" in d.message for d in all_diags), (
        "route B: message body must be preserved verbatim; got: "
        + str([d.message for d in all_diags])
    )

    # (c) Parity guard: to_dict()["message"] must equal .message (PF-007).
    for diag in all_diags:
        d_dict = diag.to_dict()
        assert isinstance(d_dict, dict), "to_dict() must return a dict"
        dict_msg = d_dict.get("message", "")
        assert dict_msg == diag.message, (
            f"to_dict()[message] must equal .message; "
            f"typed={diag.message!r}, dict={dict_msg!r}"
        )


def test_e13_lint_virtual_newline_in_frontmatter_key_escaped_on_wire() -> None:
    """T-14 / E13 [AC-F4]: WIRE-mode newline escaping on the Python surface.

    A raw newline inside a diagnostic message lets an attacker forge what reads as
    a second, independent finding in any line-oriented consumer of the value (log
    forging, YAML key injection). On the wire it must arrive as the six-character
    ``\\u000A`` literal -- and the Python surface must emit the same bytes as the
    other three surfaces: CLI, napi and WASM (PF-007).

    Reachability: a newline inside an ``@import "..."`` path is rejected by the lexer
    (that route would be vacuous, PF-013). A YAML *double-quoted* frontmatter key is
    not -- serde_yaml_ng decodes the ``\\n`` escape into a real newline, and
    ``unused-variable`` embeds the decoded key verbatim in its message.

    Verifies:
    (a) no typed ``.message`` carries a raw newline
    (b) the escaped ``\\u000A`` literal IS present (positive, non-vacuous)
    (c) the payload text survives verbatim -- escaped, not stripped
    (d) ``to_dict()`` agrees with the typed attribute (parity guard)
    """
    source = '---\n"a\\nerror[mds::forged]: FAKE\\nb": 1\n---\nHello\n'
    result = m.lint_virtual({"main.mds": source}, "main.mds")

    all_diags = [d for fr in result.files for d in fr.diagnostics]
    assert any(d.rule == "unused-variable" for d in all_diags), (
        "expected unused-variable to fire; got rules: "
        + str([d.rule for d in all_diags])
    )

    # (a) No raw newline survives into a wire message.
    for diag in all_diags:
        assert "\n" not in diag.message, (
            f"raw newline must not survive into the wire message; got: {diag.message!r}"
        )

    # (b) Positive evidence of the escape.
    assert any("\\u000A" in d.message for d in all_diags), (
        "expected \\u000A in at least one diagnostic message; got: "
        + str([d.message for d in all_diags])
    )

    # (c) Escaped, not stripped.
    assert any("error[mds::forged]" in d.message for d in all_diags), (
        "message body must be preserved verbatim; got: "
        + str([d.message for d in all_diags])
    )

    # (d) Parity: to_dict() must agree with the typed attribute (PF-007).
    for diag in all_diags:
        assert diag.to_dict()["message"] == diag.message


def test_par7_lint_result_constructor_wire_escapes_bidi_and_newline() -> None:
    """PF-004 anchor: the ``LintResult(canonical)`` / unpickle path uses WIRE mode.

    ``sanitize_lint_value()`` in ``LintResult::new()`` is a parallel path into the same
    backing store the typed getters and ``to_dict()`` read from. If it drifted to
    HUMAN mode (or missed the widened class) this constructor would become a way to
    smuggle a bidi override or a forged newline past the live-lint boundary.
    """
    raw_canonical = {
        "version": 1,
        "truncated": False,
        "files": [
            {
                "file": "fo\u202egnp.mds",
                "diagnostics": [
                    {
                        "rule": "unused-variable",
                        "severity": "warn",
                        "message": "unused \u202ekey\nerror[mds::forged]: FAKE",
                        "help": "remove \u2028 or use it",
                        "fixable": False,
                        "span": None,
                        "fix_edits": None,
                    }
                ],
            }
        ],
    }
    result = m.LintResult(raw_canonical)
    fr = result.files[0]
    diag = fr.diagnostics[0]

    _assert_no_control_chars(fr.file, "LintFileReport.file (constructor)")
    assert "\\u202E" in fr.file, f"file must be escaped; got: {fr.file!r}"

    _assert_no_control_chars(diag.message, "LintDiagnostic.message (constructor)")
    assert "\\u202E" in diag.message, f"message must escape RLO; got: {diag.message!r}"
    assert "\\u000A" in diag.message, (
        f"message must escape the newline on the wire; got: {diag.message!r}"
    )
    assert "\n" not in diag.message, (
        f"raw newline must not survive the constructor; got: {diag.message!r}"
    )
    # Escaped, not stripped.
    assert "error[mds::forged]" in diag.message

    assert diag.help is not None
    assert "\\u2028" in diag.help, f"help must escape U+2028; got: {diag.help!r}"


def test_par6_lint_result_constructor_sanitizes_typed_fields() -> None:
    """python-1 / python-2 regression anchor: LintResult(canonical) sanitizes via new().

    Constructs LintResult directly via its Python constructor (the pickle/unpickle entry
    point) with raw ESC bytes in message, help, and file fields. After the fix,
    sanitize_lint_value() runs in new() and all typed getters must return sanitized values.

    This test FAILS if sanitize_lint_value() is removed from LintResult::new()
    (avoids PF-013 — the belt-and-suspenders at the population site is vacuous without
    this constructor-level guard).
    """
    esc = "\x1b"
    raw_canonical = {
        "version": 1,
        "truncated": False,
        "files": [
            {
                "file": f"fo{esc}o.mds",
                "diagnostics": [
                    {
                        "rule": "unused-variable",
                        "severity": "warn",
                        "message": f"unused variable {esc}key",
                        "help": f"remove or use the variable {esc}key",
                        "fixable": False,
                        "span": None,
                        "fix_edits": None,
                    }
                ],
            }
        ],
    }
    result = m.LintResult(raw_canonical)
    files = result.files
    assert len(files) == 1, "expected one LintFileReport"
    fr = files[0]

    # LintFileReport.file must be sanitized.
    _assert_no_control_chars(fr.file, "LintFileReport.file (from constructor)")
    assert "\\u001B" in fr.file, f"file must contain sanitized literal; got: {fr.file!r}"

    assert len(fr.diagnostics) == 1, "expected one LintDiagnostic"
    diag = fr.diagnostics[0]

    # LintDiagnostic.message must be sanitized.
    _assert_no_control_chars(diag.message, "LintDiagnostic.message (from constructor)")
    assert "\\u001B" in diag.message, f"message must contain sanitized literal; got: {diag.message!r}"

    # LintDiagnostic.help must be sanitized.
    assert diag.help is not None
    _assert_no_control_chars(diag.help, "LintDiagnostic.help (from constructor)")
    assert "\\u001B" in diag.help, f"help must contain sanitized literal; got: {diag.help!r}"

    # Parity: to_dict()["message"] must equal .message (PF-007).
    d_dict = diag.to_dict()
    assert isinstance(d_dict, dict)
    assert d_dict["message"] == diag.message, (
        f"to_dict()[message] must equal .message; "
        f"typed={diag.message!r}, dict={d_dict['message']!r}"
    )

    # LintResult.to_dict() must also expose sanitized file key.
    result_dict = result.to_dict()
    file_in_dict = result_dict["files"][0]["file"]
    _assert_no_control_chars(file_in_dict, "to_dict() file key (from constructor)")


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
