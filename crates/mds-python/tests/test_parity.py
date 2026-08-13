"""Byte-identical parity with the shared core serializer (AC-PAR*).

The goldens below are the canonical output of the *independent* mds CLI producer
(Rust binary → mds-core), captured and checked in — they are never regenerated from
the Python binding, so the parity check is non-circular. The optional
`test_par2_live_cli_*` tests re-derive output from the CLI at run time when a binary
is available, and byte-compare it to the Python path.
"""

from __future__ import annotations

import json
import pathlib
import subprocess

import pytest

import mdscript as m
from conftest import cli_build

# (id, source, vars, expected canonical dict) — import-free so `dependencies == []`.
GOLDENS: list[tuple[str, str, dict[str, object], dict[str, object]]] = [
    (
        "plain",
        "Hello World!\n",
        {},
        {"kind": "markdown", "output": "Hello World!\n", "warnings": [], "dependencies": []},
    ),
    (
        "interp",
        "Hello {{name}}!\n",
        {"name": "World"},
        {"kind": "markdown", "output": "Hello World!\n", "warnings": [], "dependencies": []},
    ),
    (
        "empty",
        "",
        {},
        {"kind": "markdown", "output": "", "warnings": [], "dependencies": []},
    ),
    (
        "frontmatter",
        "---\nname: Alice\ncount: 3\n---\n\nHello {{name}}! You have {{count}} items.\n",
        {},
        {
            "kind": "markdown",
            "output": "---\nname: Alice\ncount: 3\n---\n\nHello Alice! You have 3 items.\n",
            "warnings": [],
            "dependencies": [],
        },
    ),
    (
        "messages",
        "@message system:\nYou are helpful.\n@end\n@message user:\nHi {{who}}!\n@end\n",
        {"who": "World"},
        {
            "kind": "messages",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "Hi World!"},
            ],
            "warnings": [],
            "dependencies": [],
        },
    ),
]


@pytest.mark.parametrize("name,src,vars,expected", GOLDENS, ids=[g[0] for g in GOLDENS])
def test_par1_to_dict_matches_golden(
    name: str, src: str, vars: dict[str, object], expected: dict[str, object]
) -> None:
    result = m.compile(src, vars=vars or None)
    # to_dict() always includes "sourceMap": None (Python-idiomatic always-present);
    # the goldens capture the wire format without it.
    assert result.to_dict() == {**expected, "sourceMap": None}
    # to_json() is the canonical wire format — matches the golden byte-for-byte.
    assert json.loads(result.to_json()) == expected


# ── PAR2: live CLI byte-for-byte cross-check (independent producer) ──────────────


def test_par2_live_cli_markdown_byte_parity(
    mds_cli: pathlib.Path, tmp_path: pathlib.Path
) -> None:
    cases = [
        ("Just some prose text.\n", []),
        ("Hello {{name}}!\n", ["--set", "name=World"]),
        ("---\ntitle: Doc\n---\n# {{title}}\n", []),
        # Blank line after frontmatter fence exercises #150/#151 interior-verbatim
        # whitespace preservation.
        ("---\ntitle: Doc\n---\n\n# {{title}}\n", []),
    ]
    for src, sets in cases:
        cli_out = cli_build(mds_cli, src, tmp_path, *sets)
        vars = {"name": "World"} if "{{name}}" in src else None
        py = m.compile(src, vars=vars).to_dict()
        assert py["kind"] == "markdown"
        assert py["output"] == cli_out, f"payload mismatch for {src!r}"


def test_par2_live_cli_messages_byte_parity(
    mds_cli: pathlib.Path, tmp_path: pathlib.Path
) -> None:
    src = "@message system:\nBe brief.\n@end\n@message user:\nHi {{who}}!\n@end\n"
    cli_out = cli_build(mds_cli, src, tmp_path, "--set", "who=Sam")
    py = m.compile(src, vars={"who": "Sam"}).to_dict()
    assert py["messages"] == json.loads(cli_out)


# ── PAR3: error code parity with the napi binding ───────────────────────────────
#
# Same inputs the napi __test__ suite asserts on must yield the same core error
# code through the Python binding (messages/spans come from the shared core).

NAPI_ERROR_PARITY = [
    ("mds::undefined_var", lambda: m.compile("Hello {{undefined_var}}!\n")),
    ("mds::syntax", lambda: m.compile("@import\n")),
    ("mds::file_not_found", lambda: m.compile_file("/no/such/file.mds")),
    ("mds::mixed_content", lambda: m.compile("Some prose text.\n\n@message user:\nA message.\n@end\n")),
    ("mds::extends", lambda: m.compile('Some text.\n@extends "./base.mds"\n')),
    ("mds::invalid_options", lambda: m.compile("Hello!\n", vars=["not", "an", "object"])),
    # Frontmatter sets count to Number(3); comparing against string literal "3" is a
    # cross-type comparison → mds::type_mismatch (#152).
    ("mds::type_mismatch", lambda: m.compile('---\ncount: 3\n---\n@if count == "3":\nx\n@end\n')),
]


@pytest.mark.parametrize(
    "code,thunk", NAPI_ERROR_PARITY, ids=[c for c, _ in NAPI_ERROR_PARITY]
)
def test_par3_error_code_parity_with_napi(code: str, thunk) -> None:  # type: ignore[no-untyped-def]
    with pytest.raises(m.MdsError) as ei:
        thunk()
    assert ei.value.code == code


# ── PAR4 / PAR5: lint canonical-JSON byte-parity (AC-API-06) ─────────────────────
#
# LINT_GOLDENS are captured from the Rust core serializer and checked in.
# They are NEVER regenerated from the Python binding, so the comparison is
# non-circular: a drift in the serializer would fail the parity test.
#
# All surfaces (Python, CLI, napi, WASM) emit keys in BTreeMap alphabetical order:
#   {"files":[...],"truncated":false,"version":1}
#
# lint_virtual() is used here to control the entry key ("main.mds") so the golden
# is byte-identical regardless of which surface produced it. lint() and lint_file()
# differ in their file key ("input.mds" vs basename) when findings are present.

LINT_GOLDENS: list[tuple[str, str, dict[str, str], str]] = [
    (
        "clean",
        "Hello World!\n",
        {},
        '{"files":[],"truncated":false,"version":1}',
    ),
    (
        "unused_variable",
        "---\nunused_key: value\n---\nHello!\n",
        {},
        (
            '{"files":[{"diagnostics":[{"fix_edits":null,"fixable":false,"help":"Remove the frontmatter'
            ' key or reference it in the template body.","message":"Variable'
            " 'unused_key' is defined in frontmatter but never referenced in the"
            ' body.","rule":"unused-variable","severity":"warn","span":{"length":10,'
            '"offset":4}}],"file":"main.mds"}],"truncated":false,"version":1}'
        ),
    ),
    (
        "silenced",
        "---\nunused_key: value\n---\nHello!\n",
        {"unused-variable": "off"},
        '{"files":[],"truncated":false,"version":1}',
    ),
]


@pytest.mark.parametrize("name,src,rules,golden", LINT_GOLDENS, ids=[g[0] for g in LINT_GOLDENS])
def test_par4_lint_virtual_matches_golden(
    name: str, src: str, rules: dict[str, str], golden: str
) -> None:
    """Python lint_virtual() must produce byte-identical JSON to the checked-in golden."""
    result = m.lint_virtual({"main.mds": src}, "main.mds", rules=rules or None)
    assert result.to_json() == golden, (
        f"lint_virtual golden mismatch for '{name}':\n"
        f"  got:    {result.to_json()}\n"
        f"  expect: {golden}"
    )


# ── PAR6: LintFileReport / LintDiagnostic .to_dict() parity when help and span are None ──
#
# PF-007 (cross-surface-parity): LintDiagnostic.as_json() previously omitted the
# "help" and "span" keys entirely when they were None, but to_canonical_json() always
# emits them as JSON null. This diverged report.to_dict() from result.to_dict()["files"][i]
# for any diagnostic with a null help or span.
#
# None of the current 9 lint rules produce null help or span in production, so the
# divergence was invisible to the existing LINT_GOLDENS. This test constructs a
# LintResult directly from a canonical dict (via LintResult.__new__) to exercise the
# null-help / null-span path explicitly, providing a non-circular differential check.


def test_par6_file_report_to_dict_parity_null_help_span() -> None:
    """LintFileReport.to_dict() and LintDiagnostic.to_dict() must match the canonical
    wire format even when help and span are None (PF-007 cross-surface-parity fix)."""
    canonical: dict[str, object] = {
        "version": 1,
        "truncated": False,
        "files": [
            {
                "file": "test.mds",
                "diagnostics": [
                    {
                        "rule": "unused-variable",
                        "severity": "warn",
                        "message": "Variable 'x' is never used.",
                        "help": None,
                        "fixable": False,
                        "span": None,
                        "fix_edits": None,
                    }
                ],
            }
        ],
    }
    result = m.LintResult(canonical)

    # file-level: LintFileReport.to_dict() must equal the canonical file dict
    report = result.files[0]
    assert report.to_dict() == result.to_dict()["files"][0], (
        "LintFileReport.to_dict() must match result.to_dict()['files'][i] "
        "when diagnostic has null help and span"
    )

    # diagnostic-level: LintDiagnostic.to_dict() must equal the canonical diagnostic dict
    diag = report.diagnostics[0]
    assert diag.to_dict() == result.to_dict()["files"][0]["diagnostics"][0], (
        "LintDiagnostic.to_dict() must match canonical diagnostic dict "
        "when help and span are None"
    )
    # Confirm the null values are present as Python None (not absent)
    diag_dict = diag.to_dict()
    assert "help" in diag_dict, "to_dict() must always include 'help' key"
    assert diag_dict["help"] is None, "help must be None (not absent) when not set"
    assert "span" in diag_dict, "to_dict() must always include 'span' key"
    assert diag_dict["span"] is None, "span must be None (not absent) when not set"
    assert "fix_edits" in diag_dict, "to_dict() must always include 'fix_edits' key"
    assert diag_dict["fix_edits"] is None, "fix_edits must be None (not absent) when not set"


def test_par5_live_cli_lint_json_parity(mds_cli: pathlib.Path, tmp_path: pathlib.Path) -> None:
    """CLI `mds lint --format json` must emit byte-identical JSON to Python lint_virtual.

    Uses a clean source (no findings) so the JSON is ``{"files":[],...}`` — byte-identical
    across surfaces without depending on the entry-key convention (basename vs "input.mds").
    """
    src = "Hello World!\n"
    mds_file = tmp_path / "main.mds"
    mds_file.write_text(src, encoding="utf-8")
    out = subprocess.run(
        [str(mds_cli), "lint", "--format", "json", str(mds_file)],
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    # CLI exits 0 for a clean file (no findings at warning/error severity).
    assert out.returncode == 0, f"CLI lint exited {out.returncode}: {out.stderr}"
    cli_json = out.stdout.strip()
    py_json = m.lint_virtual({"main.mds": src}, "main.mds").to_json()
    assert cli_json == py_json, (
        "CLI lint --format json must match Python lint_virtual byte-for-byte:\n"
        f"  CLI: {cli_json}\n"
        f"  py:  {py_json}"
    )


def test_ac_p1_24_cross_surface_diagnostic_order_parity(
    mds_cli: pathlib.Path, tmp_path: pathlib.Path
) -> None:
    """AC-P1-24 (PF-007): CLI and Python surfaces must agree on diagnostics[] order.

    Per-surface goldens each lock in their OWN ordering value and therefore cannot
    detect a cross-surface divergence (PF-007).  This test compares surfaces to each
    other — a differential, not a golden — using a multi-diagnostic fixture so that
    ordering divergence IS detectable.

    The fixture uses rules NOT touched by #203 (legacy-interpolation and
    duplicate-export), placed so the RULE-EXECUTION order differs from the OFFSET
    order.  After the AD-202-1 sort both surfaces must report the same ascending
    order.  Both surfaces sort via LintResultBuilder::build; a per-renderer
    deviation would be caught here but not by test_par4 (single-diagnostic goldens)
    or test_par5 (zero-diagnostic clean source).
    """
    # Source whose rule-execution order is the REVERSE of its offset order:
    # - legacy-interpolation (single-brace {name}) fires at a LOW offset (line 2).
    # - duplicate-export fires at a HIGH offset (end of file).
    # run_rules dispatches duplicate_export (5th) BEFORE legacy_interpolation (10th),
    # so without the sort the order is duplicate-export first — opposite of offsets.
    src = "@define greet(name):\n  Hello {name}!\n@end\n\n@export greet\n@export greet\n"
    entry_key = "parity.mds"
    mds_file = tmp_path / entry_key
    mds_file.write_text(src, encoding="utf-8")

    # CLI surface: lint the file in JSON mode.
    cli_out = subprocess.run(
        [str(mds_cli), "lint", "--format", "json", str(mds_file)],
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    # Exit 1 (warning-severity findings) or 2 (error-severity findings) are both
    # valid: legacy-interpolation is a warning, duplicate-export is an error.
    assert cli_out.returncode in (0, 1, 2), (
        f"CLI lint exited unexpectedly with {cli_out.returncode}: {cli_out.stderr}"
    )
    cli_result = json.loads(cli_out.stdout)

    # Python surface: lint_virtual with the same entry key so the "file" value matches.
    py_result = json.loads(m.lint_virtual({entry_key: src}, entry_key).to_json())

    # Non-vacuity guard: both surfaces must produce findings; an empty files[] would
    # make the ordering comparison vacuously pass (PF-013 analog).
    assert cli_result.get("files"), (
        f"CLI must produce findings for the ordering fixture; got: {cli_out.stdout}"
    )
    assert py_result.get("files"), "Python must produce findings for the ordering fixture"
    cli_diags = cli_result["files"][0].get("diagnostics", [])
    py_diags = py_result["files"][0].get("diagnostics", [])
    assert len(cli_diags) >= 2, (
        f"CLI must produce at least 2 diagnostics to test ordering; got {len(cli_diags)}"
    )
    assert len(py_diags) >= 2, (
        f"Python must produce at least 2 diagnostics to test ordering; got {len(py_diags)}"
    )

    # Normalize: strip the "file" key from each files[] entry so the comparison
    # is not skewed by per-surface file-key conventions (AC-P1-06 allows CLI and
    # bindings to differ on "file" for stdin mode; in file mode they match here).
    def drop_file_key(result: dict) -> list:  # type: ignore[type-arg]
        return [
            {k: v for k, v in f.items() if k != "file"}
            for f in result.get("files", [])
        ]

    cli_norm = drop_file_key(cli_result)
    py_norm = drop_file_key(py_result)

    assert cli_norm == py_norm, (
        "AC-P1-24 (PF-007): CLI and Python surfaces disagree on diagnostics[] content "
        "when the 'file' key is excluded.\n"
        "This indicates a per-surface ordering or shape divergence; the common choke "
        "point LintResultBuilder::build must establish the order both surfaces inherit.\n"
        f"  CLI    (file excluded): {json.dumps(cli_norm)}\n"
        f"  python (file excluded): {json.dumps(py_norm)}"
    )

    # Separately verify each surface's file key (they match in file mode).
    cli_files = [f.get("file") for f in cli_result.get("files", [])]
    py_files = [f.get("file") for f in py_result.get("files", [])]
    assert all(f == entry_key for f in cli_files), (
        f"AC-P1-24: CLI file keys must all be '{entry_key}'; got: {cli_files}"
    )
    assert all(f == entry_key for f in py_files), (
        f"AC-P1-24: Python file keys must all be '{entry_key}'; got: {py_files}"
    )


def test_par5b_live_cli_lint_differential_with_findings(mds_cli: pathlib.Path) -> None:
    """AC-P1-24: CLI stdin and Python lint() produce identical diagnostics[] (file key excluded).

    test_par5 proves byte-identity for a clean source, where the ``{"files":[]}`` result
    avoids the file-key divergence entirely.  This test covers the case WITH findings,
    where the file key differs by design — CLI stdin emits ``"<stdin>"``, Python
    ``lint()`` emits ``"input.mds"`` — and asserts cross-surface parity only on the
    fields that are *supposed* to match: ``diagnostics[]``, spans, rule, severity,
    fixable.

    Avoids PF-007: surfaces are compared to *each other* (with the file key stripped),
    not each to its own golden.  Per-surface goldens cannot catch a divergence in
    diagnostic content, span offsets, or sort order across surfaces.

    Fixture: ``{name}`` triggers ``legacy-interpolation`` at a low byte offset;
    duplicate ``@export greet`` triggers ``duplicate-export`` at a higher offset.
    ``run_rules`` dispatches ``duplicate-export`` before ``legacy-interpolation``,
    so without the offset sort the array order would be reversed.  Using this fixture
    confirms the sort ran while keeping the rule-set isolated from the #203
    span-anchoring changes.
    """
    # Source: two diagnostics whose rule-execution order is the REVERSE of offset order.
    src = "@define greet(name):\n  Hello {name}!\n@end\n\n@export greet\n@export greet\n"

    # -- CLI surface: stdin lint in JSON mode; file key = "<stdin>" --
    out = subprocess.run(
        [str(mds_cli), "lint", "-", "--format", "json"],
        input=src,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    # exit 0 = clean, exit 1 = warn-only findings, exit 2 = at least one error-severity
    # finding.  The fixture's duplicate-export rule fires at "error" severity, so exit 2
    # is the expected normal outcome (not a CLI failure).
    assert out.returncode in (0, 1, 2), (
        f"AC-P1-24: CLI lint exited unexpected status {out.returncode} "
        f"(expected 0/1/2 for a lint result):\n{out.stderr}"
    )
    assert out.returncode != 0, (
        "AC-P1-24: fixture must produce at least one finding (exit non-zero); got clean exit"
    )
    cli_result = json.loads(out.stdout.strip())

    # -- Python surface: string-source lint; file key = "input.mds" --
    py_result = json.loads(m.lint(src).to_json())

    # Normalize: strip the file key from each files[] entry (it differs by design).
    # Compare the remaining structure — diagnostics[], spans, rule, severity, fixable.
    def strip_file_key(result: dict) -> list:
        return [{k: v for k, v in entry.items() if k != "file"} for entry in result["files"]]

    cli_norm = strip_file_key(cli_result)
    py_norm = strip_file_key(py_result)

    # Non-vacuity guard: empty arrays compare equal and prove nothing (PF-013).
    total_diags = sum(len(entry.get("diagnostics", [])) for entry in cli_norm)
    assert total_diags >= 2, (
        f"AC-P1-24: fixture must produce at least two diagnostics for a meaningful "
        f"cross-surface differential; got {total_diags} from CLI"
    )

    # Cross-surface assertion: diagnostics[], spans, rule, severity, fixable must be
    # identical across surfaces when the file key is excluded (avoids PF-007).
    assert cli_norm == py_norm, (
        "AC-P1-24: CLI stdin and Python lint() diagnostics[] must be identical "
        "when the file key is excluded.\n"
        f"  CLI: {json.dumps(cli_norm)}\n"
        f"  py:  {json.dumps(py_norm)}"
    )

    # Separately assert the per-surface file key values (AC-P1-24 / AC-P1-01 / AC-P1-06).
    assert cli_result["files"][0]["file"] == "<stdin>", (
        f"AC-P1-24: CLI stdin file key must be '<stdin>'; "
        f"got '{cli_result['files'][0]['file']}'"
    )
    assert py_result["files"][0]["file"] == "input.mds", (
        f"AC-P1-24: Python string-source file key must be 'input.mds'; "
        f"got '{py_result['files'][0]['file']}'"
    )
