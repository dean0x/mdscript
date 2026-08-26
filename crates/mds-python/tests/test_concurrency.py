"""GIL-release and panic-safety behaviour (AC-PERF1, AC-PERF4).

Compilation releases the GIL around the (stateless) core via `Python::detach`, with
`catch_unwind` trapping any panic inside the released region and mapping it to
`mds::internal`. A `KeyboardInterrupt` (SIGINT) is only delivered at a Python
bytecode boundary, so it lands *after* an in-flight compile returns rather than
interrupting the native call — this is by design (the compile is a short, atomic
CPU operation).

Timing-based tests here use loose bounds and best-of-N sampling; they can still be
flaky on heavily-loaded or single-core runners — re-run before assuming a regression.

The GIL-release tests above are `@pytest.mark.perf` (wall-clock, excluded from the
blocking gate) and discard their compile results, so they prove nothing about
result *correctness* under real parallelism. The deterministic test below has no
timing assertions, so it is not perf-marked and always runs in the gating suite.
"""

from __future__ import annotations

import concurrent.futures
import os
import threading
import time

import pytest

import markdown_script as m

# A moderately expensive, deterministic compile (a few ms each).
LOOP_SRC = "@for i in items:\nItem {{i}}: lorem ipsum dolor sit amet consectetur adipiscing\n@end\n"
ITEMS = list(range(2000))


def _work() -> None:
    m.compile(LOOP_SRC, vars={"items": ITEMS})


def _best_of(fn, rounds: int = 3) -> float:  # type: ignore[no-untyped-def]
    best = float("inf")
    for _ in range(rounds):
        t0 = time.perf_counter()
        fn()
        best = min(best, time.perf_counter() - t0)
    return best


@pytest.mark.perf
@pytest.mark.skipif((os.cpu_count() or 1) < 2, reason="needs >= 2 cores for parallelism")
def test_perf1_gil_released_throughput_scaling() -> None:
    n = 32
    k = min(4, os.cpu_count() or 1)
    _work()  # warm up

    def serial() -> None:
        for _ in range(n):
            _work()

    def threaded() -> None:
        threads = [
            threading.Thread(target=lambda: [_work() for _ in range(n // k)])
            for _ in range(k)
        ]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

    t_serial = _best_of(serial)
    t_threaded = _best_of(threaded)
    # With the GIL released and >= 2 cores, K threads run in parallel and must be
    # clearly faster than fully serial. Loose bound (>= 20% faster) to tolerate noise.
    assert t_threaded < t_serial * 0.8, (
        f"no threaded speedup (serial={t_serial:.3f}s, threaded={t_threaded:.3f}s) "
        "— GIL may not be released"
    )


@pytest.mark.perf
def test_perf1_long_compile_does_not_block_main_thread() -> None:
    # While a sustained batch of native compiles runs in a background thread, the
    # main thread must keep making progress — impossible if the GIL were held for
    # the whole native call.
    done = threading.Event()
    background_iterations = 40

    def background() -> None:
        for _ in range(background_iterations):
            _work()
        done.set()

    t = threading.Thread(target=background)
    t.start()
    main_iterations = 0
    deadline = time.perf_counter() + 5.0
    while not done.is_set() and time.perf_counter() < deadline:
        m.compile("Hi {{n}}!\n", vars={"n": "x"})
        main_iterations += 1
    t.join(timeout=5.0)
    assert not t.is_alive(), "background compiles did not finish in time"
    # `main_iterations > 0` alone is near-vacuous: CPython interleaves threads at
    # Python-level call/return boundaries regardless of `Python::detach`, so even a
    # held-GIL bug yields one scheduling opportunity per background `_work()` call
    # (~background_iterations chances total). Genuine GIL release lets the main
    # thread's cheap compiles interleave far more densely than that ceiling, so
    # this floor actually discriminates the two cases instead of only proving the
    # main thread was not starved for the full 5-second deadline.
    assert main_iterations > background_iterations, (
        f"main thread made only {main_iterations} iterations (<= the "
        f"{background_iterations} background call boundaries) — GIL may not be "
        "released during the native call"
    )


# ── Deterministic concurrent correctness (gate-safe: no @pytest.mark.perf) ───────


def test_concurrent_compiles_match_single_threaded_reference() -> None:
    # Proves result *correctness* under real thread parallelism — the axis the
    # perf-marked GIL tests above can't cover (they measure wall-clock and
    # discard their compile output). Deterministic and timing-free, so unlike
    # those tests it belongs in the blocking (`-m "not perf"`) gate.
    expected = m.compile(LOOP_SRC, vars={"items": ITEMS})

    workers = min(8, (os.cpu_count() or 1) * 2)
    iterations = 64

    def _compile_once(_: int) -> m.CompileResult:
        return m.compile(LOOP_SRC, vars={"items": ITEMS})

    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        results = list(pool.map(_compile_once, range(iterations)))

    assert len(results) == iterations
    # CompileResult.__eq__ is wire equality over the full canonical value (kind,
    # output, warnings, dependencies) — so this also proves no torn/corrupted
    # read of a frozen result's backing serde_json::Value under parallelism.
    assert all(r == expected for r in results), (
        "a concurrent compile diverged from the single-threaded reference result "
        "— possible torn state under real thread parallelism"
    )


# ── PERF4: the off-GIL panic path is reserved for true panics ────────────────────


MALFORMED = [
    "{",
    "}",
    "{unclosed",
    "@if\n@end\n",
    "@for\n@end\n",
    "@message\n@end\n",
    "@define\n@end\n",
    "\x00\x01\x02",
    "@import \"\"\n",
    "{a.b.c.d.e.f.g}\n",
    "@" * 1000,
    "{" * 500 + "}" * 500,
    "———\nnot: yaml: [\n———\n",
    "@extends\n",
    "{fn(((((}\n",
]


@pytest.mark.parametrize("src", MALFORMED, ids=[repr(s)[:16] for s in MALFORMED])
def test_perf4_malformed_input_never_yields_internal(src: str) -> None:
    # Core never panics on user input, so `mds::internal` (the caught-panic code)
    # must never surface — malformed input yields a proper `mds::` error or succeeds.
    try:
        m.compile(src)
    except m.MdsError as e:
        assert e.code != "mds::internal", f"spurious internal error for {src!r}"
        assert e.code.startswith("mds::")
