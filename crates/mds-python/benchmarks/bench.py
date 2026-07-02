#!/usr/bin/env python3
"""On-demand throughput + GIL-scaling benchmark for mdscript (stdlib only).

Not part of the gated test suite. Run directly:

    python crates/mds-python/benchmarks/bench.py

Reports single-thread throughput and multi-thread scaling (which is only possible
because compilation releases the GIL).
"""

from __future__ import annotations

import os
import statistics
import threading
import time

import mdscript

REPRESENTATIVE = "---\nname: Alice\n---\n@for i in items:\n- {name}: item {i}\n@end\n"
ITEMS = list(range(200))
VARS = {"items": ITEMS}


def bench_latency(iterations: int = 2000) -> None:
    mdscript.compile(REPRESENTATIVE, vars=VARS)  # warm up
    samples = []
    for _ in range(iterations):
        t0 = time.perf_counter()
        mdscript.compile(REPRESENTATIVE, vars=VARS)
        samples.append(time.perf_counter() - t0)
    samples.sort()
    p50 = statistics.median(samples) * 1e6
    p99 = samples[int(len(samples) * 0.99)] * 1e6
    thru = iterations / sum(samples)
    print(f"latency  : p50={p50:8.1f} us   p99={p99:8.1f} us   {thru:,.0f} compiles/s")


def bench_gil_scaling(total: int = 4000) -> None:
    def run_n(n: int) -> None:
        for _ in range(n):
            mdscript.compile(REPRESENTATIVE, vars=VARS)

    t0 = time.perf_counter()
    run_n(total)
    serial = time.perf_counter() - t0

    cores = os.cpu_count() or 1
    print(f"\ncores    : {cores}")
    print(f"serial   : {total} compiles in {serial:.3f}s  ({total / serial:,.0f}/s)")
    for k in (2, 4, 8):
        if k > cores * 2:
            break
        per = total // k
        threads = [threading.Thread(target=run_n, args=(per,)) for _ in range(k)]
        t0 = time.perf_counter()
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        wall = time.perf_counter() - t0
        speedup = serial / wall if wall else float("inf")
        print(
            f"{k:>2} threads: {k * per} compiles in {wall:.3f}s  "
            f"({k * per / wall:,.0f}/s)  speedup x{speedup:.2f}"
        )


def main() -> None:
    print(f"mdscript {mdscript.__version__}\n")
    bench_latency()
    bench_gil_scaling()


if __name__ == "__main__":
    main()
