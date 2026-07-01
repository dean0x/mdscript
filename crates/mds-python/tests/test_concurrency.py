"""GIL-release and panic-safety behaviour (AC-PERF1, AC-PERF4).

Compilation releases the GIL around the (stateless) core via `Python::detach`, with
`catch_unwind` trapping any panic inside the released region and mapping it to
`mds::internal`. A `KeyboardInterrupt` (SIGINT) is only delivered at a Python
bytecode boundary, so it lands *after* an in-flight compile returns rather than
interrupting the native call — this is by design (the compile is a short, atomic
CPU operation).

Timing-based tests here use loose bounds and best-of-N sampling; they can still be
flaky on heavily-loaded or single-core runners — re-run before assuming a regression.
"""

from __future__ import annotations

import os
import threading
import time

import pytest

import mdscript as m

# A moderately expensive, deterministic compile (a few ms each).
LOOP_SRC = "@for i in items:\nItem {i}: lorem ipsum dolor sit amet consectetur adipiscing\n@end\n"
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


def test_perf1_long_compile_does_not_block_main_thread() -> None:
    # While a sustained batch of native compiles runs in a background thread, the
    # main thread must keep making progress — impossible if the GIL were held for
    # the whole native call.
    done = threading.Event()

    def background() -> None:
        for _ in range(40):
            _work()
        done.set()

    t = threading.Thread(target=background)
    t.start()
    main_iterations = 0
    deadline = time.perf_counter() + 5.0
    while not done.is_set() and time.perf_counter() < deadline:
        m.compile("Hi {n}!\n", vars={"n": "x"})
        main_iterations += 1
    t.join(timeout=5.0)
    assert not t.is_alive(), "background compiles did not finish in time"
    assert main_iterations > 0, "main thread was starved — GIL not released"


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
