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
        m.lint("Hello {undefined_var}!\n")
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
    """Each diagnostic in a LintResult has the required fields."""
    r = m.lint(UNUSED_SOURCE)
    diags = _all_diags(r)
    assert diags, "expected at least one diagnostic"
    d = diags[0]
    assert "rule" in d
    assert "severity" in d
    assert "message" in d
    assert "help" in d
    assert "fixable" in d
