"""Latency / throughput / memory-stability smoke tests (AC-PERF2, AC-PERF3).

Marked `perf`. Bounds are deliberately loose (they catch pathological regressions,
not micro-changes). Run just these with `pytest -m perf`, or skip them with
`pytest -m 'not perf'`.
"""

from __future__ import annotations

import gc
import statistics
import sys
import time

import pytest

import mdscript as m

pytestmark = pytest.mark.perf

REPRESENTATIVE = "---\nname: Alice\n---\n@for i in items:\n- {name}: item {i}\n@end\n"
ITEMS = list(range(50))


def test_perf2_representative_latency() -> None:
    m.compile(REPRESENTATIVE, vars={"items": ITEMS})  # warm up
    samples = []
    for _ in range(200):
        t0 = time.perf_counter()
        m.compile(REPRESENTATIVE, vars={"items": ITEMS})
        samples.append(time.perf_counter() - t0)
    median = statistics.median(samples)
    # Very loose: a representative compile should be well under 50 ms median.
    assert median < 0.05, f"median latency {median * 1e3:.2f} ms exceeds loose bound"


def test_perf2_large_input_throughput() -> None:
    # ~5 MiB of plain source compiles within a loose wall-clock bound.
    big = "lorem ipsum dolor sit amet\n" * (5 * 1024 * 1024 // 27)
    assert len(big) < 10 * 1024 * 1024
    t0 = time.perf_counter()
    r = m.compile(big)
    elapsed = time.perf_counter() - t0
    assert r.kind == "markdown"
    assert elapsed < 10.0, f"~5 MiB compile took {elapsed:.2f}s (loose bound 10s)"


def test_perf3_no_memory_growth_over_iterations() -> None:
    # 10k compiles must not leak Python objects unboundedly.
    src = "Hello {name}!\n"
    for _ in range(500):  # warm up allocators/caches
        m.compile(src, vars={"name": "x"})
    gc.collect()
    before = len(gc.get_objects())
    for _ in range(10_000):
        r = m.compile(src, vars={"name": "x"})
        _ = r.to_dict()
        del r
    gc.collect()
    after = len(gc.get_objects())
    growth = after - before
    # Allow slack for interpreter caches; a real leak would be ~10k+ objects.
    assert growth < 1000, f"object count grew by {growth} over 10k iterations"


def test_perf3_refcount_stability() -> None:
    # Repeated compiles must not perturb the refcount of a stable shared object.
    sentinel = "stable-vars-value"
    vars = {"name": sentinel}
    m.compile("Hello {name}!\n", vars=vars)  # warm up
    gc.collect()
    before = sys.getrefcount(sentinel)
    for _ in range(5000):
        m.compile("Hello {name}!\n", vars=vars)
    gc.collect()
    after = sys.getrefcount(sentinel)
    # The binding must not retain references to caller-provided values.
    assert abs(after - before) <= 2, f"refcount drifted {before} -> {after}"
