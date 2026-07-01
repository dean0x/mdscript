"""Pickling round-trips for results and MdsError (AC-PK1..PK3)."""

from __future__ import annotations

import pickle

import pytest

import mdscript as m

RESULTS = [
    m.compile("Hello {n}!\n", vars={"n": "A"}),
    m.compile("@message user:\nHi\n@end\n"),
    m.check("Hello {n}!\n", vars={"n": "A"}),
    m.Span(3, 5, 1, 4),
    m.Span(0, 1),  # line/column None
    m.Message("user", "hello"),
]


# ── PK1: result objects pickle round-trip (fields + equality) ───────────────────


@pytest.mark.parametrize("proto", range(2, pickle.HIGHEST_PROTOCOL + 1))
@pytest.mark.parametrize("obj", RESULTS, ids=lambda o: type(o).__name__ + repr(o)[:20])
def test_pk1_result_pickle_round_trip(obj: object, proto: int) -> None:
    back = pickle.loads(pickle.dumps(obj, protocol=proto))
    assert type(back) is type(obj)
    assert back == obj


def test_pk1_compile_result_fields_survive() -> None:
    r = m.compile("@message user:\nHi {x}\n@end\n", vars={"x": "there"})
    back = pickle.loads(pickle.dumps(r))
    assert back.kind == r.kind == "messages"
    assert back.to_dict() == r.to_dict()
    assert back.messages[0].content == r.messages[0].content  # type: ignore[index]


# ── PK2: MdsError round-trip (code / message / help / span) ──────────────────────


def test_pk2_mdserror_round_trip_with_span() -> None:
    try:
        m.compile("Hello {undef}!\n")
    except m.MdsError as e:
        back = pickle.loads(pickle.dumps(e))
        assert isinstance(back, m.MdsError)
        assert back.code == e.code == "mds::undefined_var"
        assert back.message == e.message
        assert str(back) == str(e)
        assert isinstance(back.help, str) and back.help == e.help
        assert back.span is not None and e.span is not None
        assert (back.span.offset, back.span.length, back.span.line, back.span.column) == (
            e.span.offset,
            e.span.length,
            e.span.line,
            e.span.column,
        )
    else:
        pytest.fail("expected MdsError")


def test_pk2_mdserror_round_trip_without_span() -> None:
    try:
        m.compile("x" * (10 * 1024 * 1024 + 1))
    except m.MdsError as e:
        back = pickle.loads(pickle.dumps(e))
        assert back.code == "mds::resource_limit"
        assert back.span is None
        assert back.help is None


# ── PK3: multiprocessing round-trip smoke ───────────────────────────────────────


def _mp_worker(source: str) -> object:
    """Compile in a child process and return the (picklable) result."""
    import mdscript as _m

    return _m.compile(source, vars={"name": "MP"})


def test_pk3_multiprocessing_round_trip() -> None:
    import multiprocessing as mp

    ctx = mp.get_context("spawn")  # portable across macOS/Windows/Linux
    with ctx.Pool(processes=1) as pool:
        result = pool.apply(_mp_worker, ("Hello {name}!\n",))
    # The result was pickled from the child back to the parent.
    assert isinstance(result, m.CompileResult)
    assert result.output == "Hello MP!\n"
    assert result == m.compile("Hello {name}!\n", vars={"name": "MP"})
