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
        "Hello {name}!\n",
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
        "---\nname: Alice\ncount: 3\n---\n\nHello {name}! You have {count} items.\n",
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
        "@message system:\nYou are helpful.\n@end\n@message user:\nHi {who}!\n@end\n",
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
    assert result.to_dict() == expected
    # to_json round-trips to the same value
    assert json.loads(result.to_json()) == expected


# ── PAR2: live CLI byte-for-byte cross-check (independent producer) ──────────────


def test_par2_live_cli_markdown_byte_parity(
    mds_cli: pathlib.Path, tmp_path: pathlib.Path
) -> None:
    cases = [
        ("Just some prose text.\n", []),
        ("Hello {name}!\n", ["--set", "name=World"]),
        ("---\ntitle: Doc\n---\n# {title}\n", []),
        # Blank line after frontmatter fence exercises #150/#151 interior-verbatim
        # whitespace preservation.
        ("---\ntitle: Doc\n---\n\n# {title}\n", []),
    ]
    for src, sets in cases:
        cli_out = cli_build(mds_cli, src, tmp_path, *sets)
        vars = {"name": "World"} if "{name}" in src else None
        py = m.compile(src, vars=vars).to_dict()
        assert py["kind"] == "markdown"
        assert py["output"] == cli_out, f"payload mismatch for {src!r}"


def test_par2_live_cli_messages_byte_parity(
    mds_cli: pathlib.Path, tmp_path: pathlib.Path
) -> None:
    src = "@message system:\nBe brief.\n@end\n@message user:\nHi {who}!\n@end\n"
    cli_out = cli_build(mds_cli, src, tmp_path, "--set", "who=Sam")
    py = m.compile(src, vars={"who": "Sam"}).to_dict()
    assert py["messages"] == json.loads(cli_out)


# ── PAR3: error code parity with the napi binding ───────────────────────────────
#
# Same inputs the napi __test__ suite asserts on must yield the same core error
# code through the Python binding (messages/spans come from the shared core).

NAPI_ERROR_PARITY = [
    ("mds::undefined_var", lambda: m.compile("Hello {undefined_var}!\n")),
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
# differ in their file key ("<source>" vs basename) when findings are present.

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
            '{"files":[{"diagnostics":[{"fixable":false,"help":"Remove the frontmatter'
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


def test_par5_live_cli_lint_json_parity(mds_cli: pathlib.Path, tmp_path: pathlib.Path) -> None:
    """CLI `mds lint --format json` must emit byte-identical JSON to Python lint_virtual.

    Uses a clean source (no findings) so the JSON is ``{"files":[],...}`` — byte-identical
    across surfaces without depending on the entry-key convention (basename vs "<source>").
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
