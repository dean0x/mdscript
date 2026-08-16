"""Lint surface tests — lint, lint_file, lint_virtual (AC-API-05, AC-API-06)."""

from __future__ import annotations

import json
import pathlib

import pytest

import mdscript as m

# ── Helpers ──────────────────────────────────────────────────────────────────────

CLEAN_SOURCE = "Hello World!\n"
UNUSED_SOURCE = "---\nunused_key: value\n---\nHello!\n"


def _all_diags(result: m.LintResult) -> list[dict]:
    """Flatten all per-file diagnostics from a LintResult."""
    return [d for f in result.to_dict()["files"] for d in f["diagnostics"]]


# ── lint (source string) ─────────────────────────────────────────────────────────


def test_l1_lint_clean_source_canonical_shape() -> None:
    r = m.lint(CLEAN_SOURCE)
    assert r.version == 1
    assert r.truncated is False
    files = r.to_dict()["files"]
    assert isinstance(files, list)
    assert files == [], "clean source should produce no file findings"


def test_l2_lint_accepts_none_rules() -> None:
    r = m.lint(CLEAN_SOURCE, rules=None)
    assert r.version == 1


def test_l3_lint_detects_unused_variable() -> None:
    r = m.lint(UNUSED_SOURCE)
    diags = _all_diags(r)
    rules = [d["rule"] for d in diags]
    assert "unused-variable" in rules, f"expected unused-variable; got: {rules}"


def test_l4_lint_rules_option_silences_rule() -> None:
    r = m.lint(UNUSED_SOURCE, rules={"unused-variable": "off"})
    diags = _all_diags(r)
    rules = [d["rule"] for d in diags]
    assert "unused-variable" not in rules, f"unused-variable should be silenced; got: {rules}"


def test_l5_lint_rules_unknown_severity_raises() -> None:
    with pytest.raises(m.MdsError) as ei:
        m.lint(CLEAN_SOURCE, rules={"unused-variable": "verbose"})
    assert ei.value.code == "mds::invalid_options"


def test_l6_lint_rules_non_mapping_raises() -> None:
    with pytest.raises(m.MdsError) as ei:
        m.lint(CLEAN_SOURCE, rules=["off"])  # type: ignore[arg-type]
    assert ei.value.code == "mds::invalid_options"


def test_l7_lint_invalid_source_raises() -> None:
    """The check gate fires before lint — an unresolvable variable raises MdsError."""
    with pytest.raises(m.MdsError) as ei:
        m.lint("Hello {{undefined_var}}!\n")
    assert ei.value.code.startswith("mds::")


def test_l8_lint_returns_lint_result_instance() -> None:
    r = m.lint(CLEAN_SOURCE)
    assert isinstance(r, m.LintResult)


def test_l9_lint_result_to_json_roundtrip() -> None:
    r = m.lint(UNUSED_SOURCE)
    parsed = json.loads(r.to_json())
    assert parsed["version"] == 1
    assert isinstance(parsed["files"], list)


# ── lint_file ────────────────────────────────────────────────────────────────────


def test_lf1_lint_file_clean(fixtures: pathlib.Path) -> None:
    r = m.lint_file(fixtures / "simple.mds")
    assert r.version == 1
    assert r.truncated is False


def test_lf2_lint_file_detects_findings(fixtures: pathlib.Path) -> None:
    r = m.lint_file(fixtures / "lint_warn_only.mds")
    diags = _all_diags(r)
    rules = [d["rule"] for d in diags]
    assert "unused-variable" in rules, f"expected unused-variable; got: {rules}"


def test_lf3_lint_file_rules_silences(fixtures: pathlib.Path) -> None:
    r = m.lint_file(fixtures / "lint_warn_only.mds", rules={"unused-variable": "off"})
    diags = _all_diags(r)
    rules = [d["rule"] for d in diags]
    assert "unused-variable" not in rules


def test_lf4_lint_file_str_path(fixtures: pathlib.Path) -> None:
    r = m.lint_file(str(fixtures / "simple.mds"))
    assert r.version == 1


def test_lf5_lint_file_not_found_raises() -> None:
    with pytest.raises(m.MdsError) as ei:
        m.lint_file("/nonexistent/path/no_such_file.mds")
    assert ei.value.code == "mds::file_not_found"


# ── lint_virtual ─────────────────────────────────────────────────────────────────


def test_lv1_lint_virtual_clean() -> None:
    modules = {"main.mds": CLEAN_SOURCE}
    r = m.lint_virtual(modules, "main.mds")
    assert r.version == 1
    assert r.truncated is False


def test_lv2_lint_virtual_detects_findings() -> None:
    modules = {"main.mds": UNUSED_SOURCE}
    r = m.lint_virtual(modules, "main.mds")
    diags = _all_diags(r)
    rules = [d["rule"] for d in diags]
    assert "unused-variable" in rules, f"expected unused-variable; got: {rules}"


def test_lv3_lint_virtual_rules_silences() -> None:
    modules = {"main.mds": UNUSED_SOURCE}
    r = m.lint_virtual(modules, "main.mds", rules={"unused-variable": "off"})
    diags = _all_diags(r)
    rules = [d["rule"] for d in diags]
    assert "unused-variable" not in rules


def test_lv4_lint_virtual_non_mapping_modules_raises() -> None:
    with pytest.raises(m.MdsError) as ei:
        m.lint_virtual(["main.mds"], "main.mds")  # type: ignore[arg-type]
    assert ei.value.code == "mds::invalid_options"


# ── Parity: lint and lint_file produce identical canonical JSON ──────────────────
# AC-API-06: cross-surface JSON byte-identity


def test_p_l1_lint_and_lint_file_canonical_json_identical(fixtures: pathlib.Path) -> None:
    """lint(source) and lint_file(path) produce identical JSON for clean sources.

    For sources with no findings, the JSON is ``{"files":[],"truncated":false,"version":1}``
    on all surfaces — byte-identical because the empty ``files`` array carries no filename
    keys (the file key only appears when there are diagnostics, and differs between surfaces:
    lint() uses ``"input.mds"`` while lint_file() uses the basename).
    """
    path = fixtures / "simple.mds"
    file_result = m.lint_file(path)
    source = path.read_text(encoding="utf-8")
    str_result = m.lint(source, base_path=fixtures)
    # Both must report no findings.
    assert str_result.to_json() == file_result.to_json(), (
        "lint and lint_file must produce identical canonical JSON for clean source\n"
        f"  lint:      {str_result.to_json()}\n"
        f"  lint_file: {file_result.to_json()}"
    )


# ── LintResult class contract ────────────────────────────────────────────────────


def test_lr1_lint_result_files_getter_returns_list() -> None:
    r = m.lint(UNUSED_SOURCE)
    files = r.files
    assert isinstance(files, list)
    assert len(files) >= 1, "at least one file entry expected for source with findings"


def test_lr2_lint_result_equality() -> None:
    r1 = m.lint(CLEAN_SOURCE)
    r2 = m.lint(CLEAN_SOURCE)
    assert r1 == r2


def test_lr3_lint_result_repr() -> None:
    r = m.lint(CLEAN_SOURCE)
    rep = repr(r)
    assert "LintResult" in rep
    assert "version=" in rep


def test_lr4_lint_result_diagnostic_shape() -> None:
    """Each diagnostic in a LintResult has the required fields (via to_dict)."""
    r = m.lint(UNUSED_SOURCE)
    diags = _all_diags(r)
    assert diags, "expected at least one diagnostic"
    d = diags[0]
    assert "rule" in d
    assert "severity" in d
    assert "message" in d
    assert "help" in d
    assert "fixable" in d


# ── B6/F10: Typed LintFileReport / LintDiagnostic attribute access ───────────────


def test_lr5_typed_file_report_attributes() -> None:
    """LintResult.files returns LintFileReport objects with typed attributes (B6/F10)."""
    r = m.lint(UNUSED_SOURCE)
    files = r.files
    assert isinstance(files, list)
    assert len(files) >= 1, "expected at least one LintFileReport"
    report = files[0]
    assert isinstance(report, m.LintFileReport)
    assert isinstance(report.file, str)
    assert isinstance(report.diagnostics, list)
    assert len(report.diagnostics) >= 1, "expected at least one LintDiagnostic"


def test_lr6_typed_diagnostic_attributes() -> None:
    """LintDiagnostic exposes fully-typed .rule/.severity/.message/.help/.fixable/.span."""
    r = m.lint(UNUSED_SOURCE)
    assert r.files, "expected at least one file report"
    diag = r.files[0].diagnostics[0]
    assert isinstance(diag, m.LintDiagnostic)
    # Typed attribute access (not dict indexing).
    assert isinstance(diag.rule, str) and len(diag.rule) > 0
    assert isinstance(diag.severity, str) and diag.severity in ("info", "warn", "error")
    assert isinstance(diag.message, str) and len(diag.message) > 0
    assert diag.help is None or isinstance(diag.help, str)
    assert isinstance(diag.fixable, bool)
    assert diag.span is None or isinstance(diag.span, m.Span)


def test_lr7_diagnostic_with_span() -> None:
    """A diagnostic with a span produces a typed Span object (not None)."""
    r = m.lint(UNUSED_SOURCE)
    assert r.files, "expected file reports"
    diag = r.files[0].diagnostics[0]
    # unused-variable emits a span over the frontmatter key.
    assert diag.span is not None, "unused-variable diagnostic must have a span"
    sp = diag.span
    assert isinstance(sp, m.Span)
    assert isinstance(sp.offset, int) and sp.offset >= 0
    assert isinstance(sp.length, int) and sp.length > 0
    # Lint diagnostics do not resolve line/column (SPAN-1: only offset+length in wire).
    assert sp.line is None
    assert sp.column is None


def test_lr8_lint_file_report_to_dict() -> None:
    """LintFileReport.to_dict() matches the canonical wire shape for the file entry."""
    r = m.lint_virtual({"main.mds": UNUSED_SOURCE}, "main.mds")
    assert r.files
    report = r.files[0]
    d = report.to_dict()
    assert d["file"] == "main.mds"
    assert isinstance(d["diagnostics"], list)
    assert len(d["diagnostics"]) >= 1
    diag_d = d["diagnostics"][0]
    assert "rule" in diag_d and "severity" in diag_d and "fixable" in diag_d


def test_lr9_lint_diagnostic_to_dict() -> None:
    """LintDiagnostic.to_dict() produces a canonical wire-format dict."""
    r = m.lint(UNUSED_SOURCE)
    assert r.files
    diag = r.files[0].diagnostics[0]
    d = diag.to_dict()
    assert "rule" in d
    assert "severity" in d
    assert "message" in d
    assert "fixable" in d
    # help and span are always present in to_dict() — None when not set (PF-007 fix).
    assert "help" in d
    assert "span" in d
    if diag.span is not None:
        assert d["span"] is not None
        assert "offset" in d["span"] and "length" in d["span"]
    else:
        assert d["span"] is None


def test_lr10_lint_file_report_pickle() -> None:
    """LintFileReport pickle round-trip preserves all attributes."""
    import pickle

    r = m.lint(UNUSED_SOURCE)
    assert r.files
    report = r.files[0]
    restored = pickle.loads(pickle.dumps(report))
    assert isinstance(restored, m.LintFileReport)
    assert restored == report
    assert restored.file == report.file
    assert len(restored.diagnostics) == len(report.diagnostics)


def test_lr11_lint_diagnostic_pickle() -> None:
    """LintDiagnostic pickle round-trip preserves all attributes."""
    import pickle

    r = m.lint(UNUSED_SOURCE)
    assert r.files
    diag = r.files[0].diagnostics[0]
    restored = pickle.loads(pickle.dumps(diag))
    assert isinstance(restored, m.LintDiagnostic)
    assert restored == diag
    assert restored.rule == diag.rule
    assert restored.severity == diag.severity
    assert restored.fixable == diag.fixable


def test_lr12_clean_source_empty_files() -> None:
    """Clean source produces no LintFileReport objects (files list is empty)."""
    r = m.lint(CLEAN_SOURCE)
    assert r.files == [], "clean source must produce empty files list"


# ── B6: check() rejects source_map / sources_content (Python binding) ───────────


def test_check_rejects_source_map_kwarg() -> None:
    """check() must raise MdsError(mds::invalid_options) when source_map is passed."""
    with pytest.raises(m.MdsError) as ei:
        m.check("Hello!\n", source_map=True)  # type: ignore[call-overload]
    assert ei.value.code == "mds::invalid_options"


def test_check_rejects_sources_content_kwarg() -> None:
    """check() must raise MdsError(mds::invalid_options) when sources_content is passed."""
    with pytest.raises(m.MdsError) as ei:
        m.check("Hello!\n", sources_content=True)  # type: ignore[call-overload]
    assert ei.value.code == "mds::invalid_options"


# ── to_dict() sourceMap always-present (B6/F10) ─────────────────────────────────


def test_compile_to_dict_sourcemap_none_when_absent() -> None:
    """compile().to_dict()['sourceMap'] is None when source_map was not requested."""
    r = m.compile("Hello World!\n")
    d = r.to_dict()
    assert "sourceMap" in d, "to_dict() must always include 'sourceMap'"
    assert d["sourceMap"] is None, "sourceMap must be None when not requested"


def test_compile_to_dict_sourcemap_present_when_requested() -> None:
    """compile().to_dict()['sourceMap'] is a dict when source_map=True."""
    r = m.compile("Hello World!\n", source_map=True)
    d = r.to_dict()
    assert "sourceMap" in d
    assert isinstance(d["sourceMap"], dict), "sourceMap must be a dict when generated"


# ── AC-224 D8: unknown rule name warning channel (Python) ────────────────────
#
# An unknown rule name in the `rules` kwarg produces `lint_warnings` in the
# LintResult (D8 / AC-224-1). Known rule names produce no lint_warnings.
# Python surface: lint_warnings() is a property getter that returns list[str].


def test_py_warn_l1_unknown_rule_name_returns_lint_warnings() -> None:
    """lint() with unknown rule name → lint_warnings non-empty (AC-224-1/D8)."""
    r = m.lint(CLEAN_SOURCE, rules={"no-such-rule-xyzzy": "warn"})
    assert r.version == 1
    warnings = r.lint_warnings
    assert isinstance(warnings, list), f"lint_warnings must be a list; got: {type(warnings)}"
    assert len(warnings) > 0, "lint_warnings must be non-empty for an unknown rule name"
    assert "no-such-rule-xyzzy" in warnings[0], (
        f"lint_warnings[0] must name the unknown rule; got: {warnings[0]}"
    )
    assert "recognised rules are" in warnings[0] or "recognized rules are" in warnings[0], (
        f"lint_warnings[0] must list recognised rules; got: {warnings[0]}"
    )


def test_py_warn_l2_known_rule_names_no_lint_warnings() -> None:
    """lint() with only known rule names → lint_warnings empty (AC-224-1/D8)."""
    r = m.lint(CLEAN_SOURCE, rules={"unused-variable": "off"})
    assert r.version == 1
    assert r.lint_warnings == [], (
        f"lint_warnings must be empty for known rule names; got: {r.lint_warnings}"
    )


def test_py_warn_lf1_lint_file_unknown_rule_returns_warnings(fixtures: pathlib.Path) -> None:
    """lint_file() with unknown rule name → lint_warnings non-empty (AC-224-1/D8)."""
    r = m.lint_file(fixtures / "simple.mds", rules={"no-such-rule-xyzzy": "error"})
    assert r.version == 1
    warnings = r.lint_warnings
    assert len(warnings) > 0, "lint_warnings must be non-empty for an unknown rule name"
    assert "no-such-rule-xyzzy" in warnings[0], (
        f"lint_warnings[0] must name the unknown rule; got: {warnings[0]}"
    )


def test_py_warn_lv1_lint_virtual_unknown_rule_returns_warnings() -> None:
    """lint_virtual() with unknown rule name → lint_warnings non-empty (AC-224-1/D8)."""
    modules = {"main.mds": CLEAN_SOURCE}
    r = m.lint_virtual(modules, "main.mds", rules={"no-such-rule-xyzzy": "warn"})
    assert r.version == 1
    warnings = r.lint_warnings
    assert len(warnings) > 0, "lint_warnings must be non-empty for an unknown rule name"
    assert "no-such-rule-xyzzy" in warnings[0], (
        f"lint_warnings[0] must name the unknown rule; got: {warnings[0]}"
    )


def test_py_warn_l3_lint_continues_with_unknown_rule() -> None:
    """lint() with unknown rule name still returns files[] and truncated (AC-224-1/D8)."""
    r = m.lint(CLEAN_SOURCE, rules={"no-such-rule-xyzzy": "warn"})
    d = r.to_dict()
    assert "files" in d, "files key must be present even when unknown rule name is given"
    assert "truncated" in d, "truncated key must be present even when unknown rule name is given"
    assert d["truncated"] is False


def test_py_warn_l4_to_dict_includes_lint_warnings() -> None:
    """lint() with unknown rule name → to_dict() includes 'lint_warnings' key (AC-224-1/D8)."""
    r = m.lint(CLEAN_SOURCE, rules={"no-such-rule-xyzzy": "warn"})
    d = r.to_dict()
    assert "lint_warnings" in d, (
        f"to_dict() must include 'lint_warnings' when unknown rule names are present; got: {d}"
    )
    assert isinstance(d["lint_warnings"], list)
    assert len(d["lint_warnings"]) > 0


def test_py_warn_l5_multiple_unknown_rules() -> None:
    """lint() with multiple unknown rule names → both appear sorted lexicographically (AC-224-3).

    Names are deliberately supplied in reverse lexicographic order ('zzz-bad' before
    'aaa-bad') to verify that UnknownRuleNames::new sorts the offender list rather
    than emitting it in HashMap iteration order (AC-224-3).
    """
    r = m.lint(
        CLEAN_SOURCE,
        rules={"zzz-bad": "warn", "aaa-bad": "error"},
    )
    warnings = r.lint_warnings
    assert len(warnings) > 0, "lint_warnings must be non-empty"
    combined = " ".join(warnings)
    assert "zzz-bad" in combined, (
        f"all unknown rule names must appear in lint_warnings; got: {warnings}"
    )
    assert "aaa-bad" in combined, (
        f"all unknown rule names must appear in lint_warnings; got: {warnings}"
    )
    # AC-224-3: offenders MUST be sorted lexicographically (not HashMap iteration order).
    # 'aaa-bad' < 'zzz-bad', so 'aaa-bad' must appear first in the combined warning text.
    idx_aaa = combined.index("aaa-bad")
    idx_zzz = combined.index("zzz-bad")
    assert idx_aaa < idx_zzz, (
        f"offenders must be sorted lexicographically: 'aaa-bad' must precede 'zzz-bad' "
        f"(AC-224-3); got: {warnings}"
    )


def test_py_warn_l2b_known_rule_to_dict_omits_lint_warnings_key() -> None:
    """lint() with only known rules → to_dict() omits 'lint_warnings' key entirely.

    Pins the absent-when-empty convention: r.lint_warnings == [] and
    'lint_warnings' not in r.to_dict() both describe the same "no warnings" state.
    Positive control: test_py_warn_l4 asserts the key IS present when warnings fired,
    so this absence assertion is not vacuous (ADR-009 / PF-013).
    """
    r = m.lint(CLEAN_SOURCE, rules={"unused-variable": "off"})
    assert r.lint_warnings == [], "lint_warnings attribute must be empty for known rules"
    d = r.to_dict()
    assert "lint_warnings" not in d, (
        "to_dict() must omit 'lint_warnings' when no warnings occurred "
        f"(absent-when-empty convention); got keys: {list(d.keys())}"
    )


def test_py_warn_canonical_sanitizes_lint_warnings() -> None:
    """LintResult(canonical) sanitizes hostile control bytes in lint_warnings (PF-004).

    The live lint path is safe because format_unknown_rule_names_warning WIRE-escapes
    strings before injection. The LintResult(canonical) / pickle path must also sanitize
    lint_warnings because callers may supply untrusted canonical data.

    Positive control: construct an ESC byte at runtime (not as a literal, per PF-018)
    and confirm the getter does NOT return the raw byte after construction.
    """
    esc = chr(0x1B)  # ESC — constructed at runtime, not written as a literal (PF-018)
    raw_warning = f"unknown rule {esc}[31mhostile{esc}[0m"
    canonical = {
        "version": 1,
        "files": [],
        "truncated": False,
        "lint_warnings": [raw_warning],
    }
    result = m.LintResult(canonical)
    warnings = result.lint_warnings
    assert len(warnings) == 1, f"expected one warning entry; got: {warnings}"
    assert esc not in warnings[0], (
        f"ESC byte must be sanitized in lint_warnings via LintResult(canonical); "
        f"got: {warnings[0]!r}"
    )
    # The sanitized form must be non-empty: hostile bytes are replaced, not dropped.
    assert len(warnings[0]) > 0, "sanitized warning must be non-empty"
    # WIRE-escaping must replace raw ESC with its JSON escape sequence (6 ASCII chars).
    # Construct the expected string at runtime to avoid authoring a literal control byte
    # inside a string constant (PF-018: the tooling decodes backslash-u + 4 hex to real bytes).
    expected_escape = "\\u001B"  # backslash + u + 0 + 0 + 1 + B — six ASCII characters
    assert expected_escape in warnings[0], (
        f"ESC must appear as the escaped literal \\u001B; got: {warnings[0]!r}"
    )


def test_py_warn_live_lint_escapes_hostile_rule_name() -> None:
    """live lint() path: a hostile rule name (ESC byte) must not reach lint_warnings raw.

    ADR-008 per-surface: the mds-core unit test warning_wire_escapes_hostile_rule_name
    proves the formatter escapes before building the string. This test proves the string
    that actually reaches Python through the PyO3 binding carries no raw control byte
    (negative) and DOES carry the sanitized literal (positive control, PF-013/ADR-009).

    PF-018: the ESC byte is constructed at runtime via chr() — never authored as a literal.
    """
    esc = chr(0x1B)  # U+001B — runtime construction only (PF-018)
    hostile_rule = esc + "[31mhostile-rule" + esc + "[0m"
    r = m.lint(CLEAN_SOURCE, rules={hostile_rule: "warn"})

    # The call must succeed and expose the warning through lint_warnings (D8 / AC-224-1).
    warnings = r.lint_warnings
    assert isinstance(warnings, list), f"lint_warnings must be a list; got: {type(warnings)}"
    assert len(warnings) > 0, "lint_warnings must be non-empty for a hostile unknown rule name"

    w0 = warnings[0]
    assert isinstance(w0, str), f"lint_warnings[0] must be a str; got: {type(w0)}"

    # Negative: no raw C0 (excl. \t \n), DEL, or C1 byte may survive (ADR-008).
    for i, ch in enumerate(w0):
        code = ord(ch)
        is_c0 = code < 0x20 and code not in (0x09, 0x0A)
        is_del = code == 0x7F
        is_c1 = 0x80 <= code <= 0x9F
        assert not (is_c0 or is_del or is_c1), (
            f"raw hostile char U+{code:04X} at index {i} must not appear in "
            f"lint_warnings[0]; got: {w0!r}"
        )

    # Positive control (PF-013/ADR-009): the sanitized literal must be present so
    # the negative above cannot pass merely because the name never reached the message.
    # Construct the expected 6-char string at runtime (PF-018).
    expected_escape = "\\u001B"  # backslash + u + 0 + 0 + 1 + B
    assert expected_escape in w0, (
        f"sanitized \\u001B literal must appear in lint_warnings[0]; got: {w0!r}"
    )
