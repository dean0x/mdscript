"""Shared pytest fixtures for the mdscript binding suite."""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

HERE = Path(__file__).parent
FIXTURES = HERE / "fixtures"
# tests/ -> mds-python/ -> crates/ -> repo root
REPO_ROOT = HERE.parents[2]


@pytest.fixture(scope="session")
def fixtures() -> Path:
    """Directory of `.mds` fixture files bundled with the Python tests."""
    return FIXTURES


def _find_cli() -> Path | None:
    """Locate a built `mds` CLI binary (the independent parity producer).

    Priority:
    1. ``MDS_CLI_BIN`` environment variable — if set, the path must exist as a
       file; a non-existent or non-file path raises :class:`FileNotFoundError`
       immediately rather than silently falling through to other candidates.
    2. The *freshest* of ``target/release/mds`` and ``target/debug/mds`` by
       mtime — prefers the binary that was compiled most recently so a fresh
       debug build is not shadowed by a stale release artifact.
    3. ``mds`` found anywhere on ``$PATH`` via :func:`shutil.which`.
    """
    env = os.environ.get("MDS_CLI_BIN")
    if env:
        p = Path(env)
        if not p.is_file():
            raise FileNotFoundError(
                f"MDS_CLI_BIN={env!r} is set but points to a non-existent or "
                "non-file path; remove it or correct the path"
            )
        return p
    exe = "mds.exe" if os.name == "nt" else "mds"
    candidates = [
        REPO_ROOT / "target" / profile / exe for profile in ("release", "debug")
    ]
    existing = [c for c in candidates if c.is_file()]
    if existing:
        # Pick whichever binary was modified most recently; ties resolve to
        # release (candidates[0]) because Python's max() returns the first
        # maximum encountered when keys are equal.
        return max(existing, key=lambda p: p.stat().st_mtime)
    found = shutil.which("mds")
    return Path(found) if found else None


@pytest.fixture(scope="session")
def mds_cli() -> Path:
    """Path to the `mds` CLI, or skip if it is not available.

    The CLI is a *separate* code path (Rust binary → mds-core) from the Python
    FFI binding, so using it to produce golden output keeps parity checks
    non-circular. It is optional: the suite's hard-coded goldens cover parity on
    their own, and this fixture only enables the extra live cross-check.
    """
    cli = _find_cli()
    if cli is None:
        pytest.skip("mds CLI binary not found (set MDS_CLI_BIN or build mds-cli)")
    return cli


def cli_build(cli: Path, source: str, tmp_path: Path, *sets: str) -> str:
    """Compile `source` through the CLI and return its raw stdout (the payload)."""
    src = tmp_path / "parity.mds"
    src.write_text(source, encoding="utf-8")
    cmd = [str(cli), "build", str(src), "-o", "-", *sets]
    out = subprocess.run(
        cmd, capture_output=True, text=True, check=True, encoding="utf-8"
    )
    return out.stdout
