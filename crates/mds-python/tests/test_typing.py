"""Static typing: `.pyi` + `py.typed` clean under mypy --strict and pyright (AC-C6)."""

from __future__ import annotations

import importlib.util
import subprocess
import sys
from pathlib import Path

import pytest

SAMPLE = Path(__file__).parent / "typecheck_sample.py"


def test_c6_py_typed_and_stubs_installed() -> None:
    import mdscript

    pkg = Path(mdscript.__file__).parent
    assert (pkg / "py.typed").is_file(), "py.typed marker must ship in the package"
    assert (pkg / "_mdscript.pyi").is_file(), "extension stub must ship"
    assert (pkg / "__init__.pyi").is_file(), "package stub must ship"


@pytest.mark.skipif(
    importlib.util.find_spec("mypy") is None, reason="mypy not installed"
)
def test_c6_mypy_strict_clean() -> None:
    proc = subprocess.run(
        [sys.executable, "-m", "mypy", "--strict", "--no-incremental", str(SAMPLE)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, f"mypy --strict failed:\n{proc.stdout}\n{proc.stderr}"


@pytest.mark.skipif(
    importlib.util.find_spec("pyright") is None, reason="pyright not installed"
)
def test_c6_pyright_clean() -> None:
    proc = subprocess.run(
        [sys.executable, "-m", "pyright", str(SAMPLE)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, f"pyright failed:\n{proc.stdout}\n{proc.stderr}"
