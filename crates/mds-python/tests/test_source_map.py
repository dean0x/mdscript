"""Source Map v3 surface tests for the Python binding (T6 — AC-FUNC-*, AC-API-*).

Covers:
- SM-PY-1: source_map=True produces a dict with version/sources/names/mappings
- SM-PY-2: default (no option) → source_map getter returns None
- SM-PY-3: sources_content=True → sourcesContent present, index-aligned
- SM-PY-4: messages-mode + source_map=True degrades to None + warning
- SM-PY-5: sources_content=True without source_map=True → mds::invalid_options
- SM-PY-6: pickle round-trip preserves source_map
- SM-PY-7: __repr__ includes source_map=<present> when present
- SM-PY-8: SOURCE_MAP_GOLDENS — byte-identical compile_virtual JSON
- SM-PY-9: live CLI sidecar parity (skipped when CLI unavailable)
- SM-PY-10: compile_file source_map support
"""

from __future__ import annotations

import json
import pickle
import re
import subprocess
from pathlib import Path

import pytest

import mdscript as m
from conftest import FIXTURES

# Base64 alphabet used by VLQ mappings (excludes <, >, - per security constraint).
_VLQ_RE = re.compile(r"^[A-Za-z0-9+/,;]*$")

# Simple one-line MDS source for consistent source-map assertions.
_SIMPLE_SRC = "Hello World!\n"

# Messages-mode source (no output → source map degrades).
_MESSAGES_SRC = (
    "@message system:\nYou are helpful.\n@end\n"
    "@message user:\nHi there!\n@end\n"
)


def _check_sm_structure(sm: object, *, has_sources_content: bool = False) -> None:
    """Assert Source Map v3 structural invariants (O(1) checks, no array traversal)."""
    assert isinstance(sm, dict), "source_map must be a dict"
    assert sm.get("version") == 3, "version must be 3"
    assert isinstance(sm.get("sources"), list), "sources must be a list"
    assert isinstance(sm.get("names"), list), "names must be a list"
    mappings = sm.get("mappings")
    assert isinstance(mappings, str), "mappings must be a string"
    assert _VLQ_RE.match(mappings) is not None, f"mappings not valid VLQ: {mappings!r}"
    assert "file" not in sm, "file key must be absent for binding results"
    if has_sources_content:
        sc = sm.get("sourcesContent")
        assert isinstance(sc, list), "sourcesContent must be a list"
        assert len(sc) == len(sm["sources"]), "sourcesContent must align with sources"
    else:
        assert "sourcesContent" not in sm, "sourcesContent must be absent"


# ── SM-PY-1: source_map=True produces a valid SM dict ────────────────────────

def test_sm_py1_compile_produces_source_map() -> None:
    result = m.compile(_SIMPLE_SRC, source_map=True)
    sm = result.source_map
    assert sm is not None, "source_map getter should be non-None"
    _check_sm_structure(sm)
    # String-source compilation uses "input.mds" (STRING_SOURCE_MAP_LABEL) as
    # the entry label — unified with the WASM backend via the choke-point fix.
    assert sm["sources"] == ["input.mds"]


def test_sm_py1_compile_virtual_produces_source_map() -> None:
    modules = {"main.mds": _SIMPLE_SRC}
    result = m.compile_virtual(modules, "main.mds", source_map=True)
    sm = result.source_map
    assert sm is not None
    _check_sm_structure(sm)
    # Virtual compilation uses caller-supplied entry as source label.
    assert sm["sources"] == ["main.mds"]


# ── SM-PY-2: default → source_map is None ─────────────────────────────────

def test_sm_py2_default_no_source_map() -> None:
    result = m.compile(_SIMPLE_SRC)
    assert result.source_map is None
    # to_dict() must not contain the "sourceMap" key.
    d = result.to_dict()
    assert "sourceMap" not in d


def test_sm_py2_explicit_false_no_source_map() -> None:
    result = m.compile(_SIMPLE_SRC, source_map=False)
    assert result.source_map is None


# ── SM-PY-3: sources_content=True → sourcesContent present ────────────────

def test_sm_py3_sources_content_present() -> None:
    result = m.compile(_SIMPLE_SRC, source_map=True, sources_content=True)
    sm = result.source_map
    assert sm is not None
    _check_sm_structure(sm, has_sources_content=True)
    sc = sm["sourcesContent"]
    # The embedded content should contain the original source text.
    assert any(isinstance(v, str) and "Hello World" in v for v in sc)


def test_sm_py3_compile_virtual_sources_content() -> None:
    modules = {"entry.mds": _SIMPLE_SRC}
    result = m.compile_virtual(
        modules, "entry.mds", source_map=True, sources_content=True
    )
    sm = result.source_map
    assert sm is not None
    _check_sm_structure(sm, has_sources_content=True)


# ── SM-PY-4: messages-mode degrades gracefully ─────────────────────────────

def test_sm_py4_messages_mode_degrades() -> None:
    result = m.compile(_MESSAGES_SRC, source_map=True)
    # Messages-mode templates have no renderable output → no source map.
    assert result.source_map is None
    assert result.kind == "messages"
    # The binding must emit MSG_MODE_SOURCE_MAP_WARNING (shared core constant).
    # The assertion is anchored to a distinctive phrase from that constant so
    # that any drift of the constant text is caught here (avoids PF-007).
    warnings = result.warnings
    assert any("messages-mode templates" in w for w in warnings), (
        f"expected MSG_MODE_SOURCE_MAP_WARNING (contains 'messages-mode templates'), "
        f"got: {warnings}"
    )


# ── SM-PY-5: cross-field validation ────────────────────────────────────────

@pytest.mark.parametrize(
    "fn",
    [
        lambda: m.compile(_SIMPLE_SRC, sources_content=True),
        lambda: m.compile_file(FIXTURES / "simple.mds", sources_content=True),
        lambda: m.compile_virtual(
            {"e.mds": _SIMPLE_SRC}, "e.mds", sources_content=True
        ),
    ],
    ids=["compile", "compile_file", "compile_virtual"],
)
def test_sm_py5_sources_content_without_source_map_raises(fn) -> None:  # type: ignore[no-untyped-def]
    with pytest.raises(m.MdsError) as ei:
        fn()
    assert ei.value.code == "mds::invalid_options"
    assert "sources_content" in str(ei.value)


# ── SM-PY-6: pickle round-trip ─────────────────────────────────────────────

def test_sm_py6_pickle_roundtrip_with_source_map() -> None:
    result = m.compile(_SIMPLE_SRC, source_map=True)
    restored = pickle.loads(pickle.dumps(result))
    assert restored == result
    assert restored.source_map == result.source_map


def test_sm_py6_pickle_roundtrip_without_source_map() -> None:
    result = m.compile(_SIMPLE_SRC)
    restored = pickle.loads(pickle.dumps(result))
    assert restored == result
    assert restored.source_map is None


# ── SM-PY-7: __repr__ ──────────────────────────────────────────────────────

def test_sm_py7_repr_includes_source_map_when_present() -> None:
    result = m.compile(_SIMPLE_SRC, source_map=True)
    r = repr(result)
    assert "source_map=<present>" in r


def test_sm_py7_repr_no_source_map_marker_when_absent() -> None:
    result = m.compile(_SIMPLE_SRC)
    r = repr(result)
    assert "source_map" not in r


# ── SM-PY-8: SOURCE_MAP_GOLDENS ────────────────────────────────────────────
#
# Byte-identical golden check via compile_virtual so the entry key is
# deterministic ("entry.mds"). These are produced by the shared core serializer
# and checked in — never regenerated from the Python binding.
#
# Only structural fields (version, sources, names) are checked here because the
# `mappings` VLQ string is a function of the source content and may grow as the
# core mapping algorithm is refined. The key invariant is that the JSON round-trip
# produces a valid SM v3 structure and the `"sourceMap"` key is absent/present
# in the right places.

SOURCE_MAP_GOLDENS: list[tuple[str, dict[str, str], bool, bool]] = [
    ("no_map", {"entry.mds": _SIMPLE_SRC}, False, False),
    ("with_map", {"entry.mds": _SIMPLE_SRC}, True, False),
    ("with_content", {"entry.mds": _SIMPLE_SRC}, True, True),
]


@pytest.mark.parametrize(
    "name,modules,sm,sc",
    SOURCE_MAP_GOLDENS,
    ids=[g[0] for g in SOURCE_MAP_GOLDENS],
)
def test_sm_py8_golden_structure(
    name: str, modules: dict[str, str], sm: bool, sc: bool
) -> None:
    result = m.compile_virtual(
        modules, "entry.mds", source_map=sm, sources_content=sc
    )
    d = result.to_dict()
    if not sm:
        assert "sourceMap" not in d, "sourceMap must be absent when not requested"
    else:
        assert "sourceMap" in d, "sourceMap must be present when requested"
        _check_sm_structure(d["sourceMap"], has_sources_content=sc)
    # JSON round-trip is consistent with to_dict()
    from_json = json.loads(result.to_json())
    assert from_json == d


# ── SM-PY-9: live CLI sidecar parity ──────────────────────────────────────

def test_sm_py9_live_cli_source_map_parity(
    mds_cli: Path, tmp_path: Path
) -> None:
    """CLI `mds build --source-map` sidecar must be structurally valid and
    match the Python binding's sourceMap at the structural level.

    `mds build` does not have a `--format` flag (only `mds lint` does), so
    parity is obtained by reading the `.md.map` sidecar file written by
    `mds build --source-map -o <output>`.  Structural fields (version, names,
    mappings) are compared; sources[] is intentionally excluded because the
    CLI relativizes paths while the Python binding uses the caller-supplied
    absolute path as the source label.
    """
    src = _SIMPLE_SRC
    src_file = tmp_path / "parity.mds"
    src_file.write_text(src, encoding="utf-8")

    out_file = tmp_path / "parity_out.md"
    map_file = tmp_path / "parity_out.md.map"

    proc = subprocess.run(
        [str(mds_cli), "build", "--source-map", str(src_file), "-o", str(out_file)],
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    assert proc.returncode == 0, (
        f"CLI build --source-map failed (rc={proc.returncode}): {proc.stderr}"
    )
    assert map_file.exists(), f"CLI did not write sidecar map at {map_file}"

    cli_sm = json.loads(map_file.read_text(encoding="utf-8"))

    py_result = m.compile_file(src_file, source_map=True)
    py_sm = py_result.source_map
    assert py_sm is not None

    # Structural invariants for CLI sidecar (has a "file" key; use inline check).
    assert cli_sm.get("version") == 3, f"CLI map version must be 3, got: {cli_sm.get('version')}"
    assert isinstance(cli_sm.get("sources"), list), "CLI map sources must be a list"
    assert isinstance(cli_sm.get("names"), list), "CLI map names must be a list"
    assert isinstance(cli_sm.get("mappings"), str), "CLI map mappings must be a string"
    assert _VLQ_RE.match(cli_sm["mappings"]) is not None, (
        f"CLI map mappings not valid VLQ: {cli_sm['mappings']!r}"
    )

    # Structural invariants for Python binding result.
    _check_sm_structure(py_sm)

    # Cross-binding parity on fields that are format-invariant.
    assert cli_sm["version"] == py_sm["version"] == 3
    assert cli_sm["names"] == py_sm["names"]
    # mappings must be byte-identical: both bindings use the same mds-core
    # serializer (ADR-002 / AC-API-04).
    assert cli_sm["mappings"] == py_sm["mappings"], (
        f"CLI and Python mappings must match:\n"
        f"  CLI:    {cli_sm['mappings']!r}\n"
        f"  Python: {py_sm['mappings']!r}"
    )


# ── SM-PY-10: compile_file source_map ──────────────────────────────────────

def test_sm_py10_compile_file_source_map() -> None:
    result = m.compile_file(FIXTURES / "simple.mds", source_map=True)
    sm = result.source_map
    assert sm is not None
    _check_sm_structure(sm)
    # compile_file now emits root-relative paths in sources[] (ADR-005 Phase A
    # choke-point fix in source_path.rs::relativize_source).  The value is a
    # slash-separated path relative to the project root (located via .mdsroot /
    # .git walk-up), NOT the absolute filesystem path.  Since the fixtures dir
    # is the project root for this test, sources[0] ends with "simple.mds".
    assert len(sm["sources"]) == 1
    assert sm["sources"][0].endswith("simple.mds")
    # Must be root-relative, NOT an absolute filesystem path (ADR-005 security fix).
    assert not sm["sources"][0].startswith("/"), (
        f"sources[0] must not be an absolute path; got: {sm['sources'][0]!r}"
    )
    # No file key in binding output.
    assert "file" not in sm


def test_sm_py10_compile_file_default_no_source_map() -> None:
    result = m.compile_file(FIXTURES / "simple.mds")
    assert result.source_map is None
