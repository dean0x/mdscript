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
            "output": "---\nname: Alice\ncount: 3\n---\nHello Alice! You have 3 items.\n",
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
]


@pytest.mark.parametrize(
    "code,thunk", NAPI_ERROR_PARITY, ids=[c for c, _ in NAPI_ERROR_PARITY]
)
def test_par3_error_code_parity_with_napi(code: str, thunk) -> None:  # type: ignore[no-untyped-def]
    with pytest.raises(m.MdsError) as ei:
        thunk()
    assert ei.value.code == code
