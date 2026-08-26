"""Static typing: `.pyi` + `py.typed` clean under mypy --strict and pyright (AC-C6)."""

from __future__ import annotations

import importlib.util
import subprocess
import sys
from pathlib import Path

import pytest

SAMPLE = Path(__file__).parent / "typecheck_sample.py"
# Pyright project root: the mds-python package directory (contains pyrightconfig.json
# with extraPaths pointing to ./python so "import markdown_script" resolves).
_PYRIGHT_PROJECT = Path(__file__).parent.parent

# Known, structurally-justified stub/runtime diffs `stubtest` cannot resolve
# statically — not stub bugs. Substring-matched against each `error:` line so a
# minor mypy wording change doesn't break the filter. `MdsError.code/message/
# help/span` are attached via `setattr` on each raised instance
# (src/lib.rs `mds_err_to_py`/`coded_error`), never as class-level descriptors, so
# static introspection of the class object can never see them.
_KNOWN_STUBTEST_DIFFS = (
    "markdown_script.MdsError.code",
    "markdown_script.MdsError.message",
    "markdown_script.MdsError.help",
    "markdown_script.MdsError.span",
)


def test_c6_py_typed_and_stubs_installed() -> None:
    import markdown_script

    pkg = Path(markdown_script.__file__).parent
    assert (pkg / "py.typed").is_file(), "py.typed marker must ship in the package"
    assert (pkg / "_markdown_script.pyi").is_file(), "extension stub must ship"
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
        [sys.executable, "-m", "pyright", "--project", str(_PYRIGHT_PROJECT), str(SAMPLE)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, f"pyright failed:\n{proc.stdout}\n{proc.stderr}"


@pytest.mark.skipif(
    importlib.util.find_spec("mypy") is None, reason="mypy not installed"
)
def test_c6_stubtest_matches_runtime() -> None:
    # `mypy --strict`/pyright above only check a hand-written sample — they never
    # compare the shipped `.pyi` against the actual PyO3 runtime module, so a
    # renamed getter or a missing constructor (see CompileResult.__init__) ships
    # undetected. `stubtest` diffs the stub against the imported extension directly.
    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "mypy.stubtest",
            "markdown_script",
            "--ignore-missing-stub",  # runtime-only dunders (__repr__, __reduce__, …)
        ],
        capture_output=True,
        text=True,
    )
    # stubtest exits 0 (clean) or 1 (diffs found); anything else means it never
    # actually ran (bad invocation, import failure) and must not read as a pass.
    assert proc.returncode in (0, 1), (
        f"mypy.stubtest did not run cleanly (exit {proc.returncode}):\n"
        f"{proc.stdout}\n{proc.stderr}"
    )
    unexpected = [
        line
        for line in proc.stdout.splitlines()
        if line.startswith("error:")
        and not any(known in line for known in _KNOWN_STUBTEST_DIFFS)
    ]
    assert not unexpected, (
        "unexpected stub/runtime drift:\n"
        + "\n".join(unexpected)
        + f"\n\nfull output:\n{proc.stdout}\n{proc.stderr}"
    )
