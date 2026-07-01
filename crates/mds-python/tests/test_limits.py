"""Boundary guards and vars value edges (AC-L*, AC-V3)."""

from __future__ import annotations

import math

import pytest

import mdscript as m

MAX = 10 * 1024 * 1024  # MAX_SOURCE_SIZE (10 MiB)


# ── L1: >10 MiB source → resource_limit (all string inputs) ─────────────────────


def test_l1_oversized_source_compile() -> None:
    with pytest.raises(m.MdsError) as ei:
        m.compile("x" * (MAX + 1))
    assert ei.value.code == "mds::resource_limit"


def test_l1_oversized_source_check_and_scan() -> None:
    big = "x" * (MAX + 1)
    for fn in (m.check, m.scan_imports):
        with pytest.raises(m.MdsError) as ei:
            fn(big)
        assert ei.value.code == "mds::resource_limit"


def test_l1_source_at_exactly_limit_not_rejected_by_guard() -> None:
    # Exactly at the limit must not trip the size guard (strictly-greater rejects).
    try:
        r = m.compile(" " * MAX)
        assert isinstance(r.output, str)
    except m.MdsError as e:
        assert e.code != "mds::resource_limit"


# ── L2: virtual count / aggregate size / entry∉modules ──────────────────────────


def test_l2_too_many_modules() -> None:
    mods = {f"m{i}.mds": "x" for i in range(257)}
    with pytest.raises(m.MdsError) as ei:
        m.compile_virtual(mods, "m0.mds")
    assert ei.value.code == "mds::resource_limit"


def test_l2_single_module_over_size() -> None:
    with pytest.raises(m.MdsError) as ei:
        m.compile_virtual({"a.mds": "x" * (MAX + 1)}, "a.mds")
    assert ei.value.code == "mds::resource_limit"


def test_l2_aggregate_over_size() -> None:
    # Each module is under the per-module cap, but together they exceed it.
    chunk = "x" * (4 * 1024 * 1024)
    mods = {"a.mds": chunk, "b.mds": chunk, "c.mds": chunk}  # ~12 MiB total
    with pytest.raises(m.MdsError) as ei:
        m.compile_virtual(mods, "a.mds")
    assert ei.value.code == "mds::resource_limit"


def test_l2_entry_not_in_modules_is_mdserror() -> None:
    with pytest.raises(m.MdsError) as ei:
        m.compile_virtual({"a.mds": "hi\n"}, "missing.mds")
    # resolution failure — a real core error, not a boundary/options error
    assert ei.value.code.startswith("mds::")
    assert ei.value.code not in ("mds::invalid_options", "mds::internal")


def test_l2_non_mapping_and_bad_value_modules() -> None:
    with pytest.raises(m.MdsError) as ei:
        m.compile_virtual([1, 2, 3], "a.mds")  # type: ignore[arg-type]
    assert ei.value.code == "mds::invalid_options"
    with pytest.raises(m.MdsError) as ei:
        m.compile_virtual({"a.mds": 123}, "a.mds")  # type: ignore[dict-item]
    assert ei.value.code == "mds::invalid_options"


# ── L3: core structural limits surface as MdsError, not a panic ──────────────────


def test_l3_deep_nesting_is_mdserror_not_panic() -> None:
    deep = "@if true:\n" * 70 + "x\n" + "@end\n" * 70
    with pytest.raises(m.MdsError) as ei:
        m.compile(deep)
    assert ei.value.code.startswith("mds::")
    assert ei.value.code != "mds::internal"  # a clean structural error, not a panic


def test_l3_deep_value_nesting_is_mdserror() -> None:
    nested: object = "leaf"
    for _ in range(70):
        nested = {"a": nested}
    with pytest.raises(m.MdsError) as ei:
        m.compile("{v}\n", vars={"v": nested})
    assert ei.value.code in ("mds::invalid_options", "mds::json")


# ── V3: vars value edges — defined outcomes + numeric parity vs napi ─────────────


def test_v3_plain_numbers() -> None:
    assert m.compile("{v}\n", vars={"v": 42}).output == "42\n"
    assert m.compile("{v}\n", vars={"v": 1.5}).output == "1.5\n"


def test_v3_float_1e20_parity_with_napi() -> None:
    # JS numbers are f64; napi renders `1e20` as an integer string. A Python float
    # goes through the same f64 path, so parity holds exactly.
    assert m.compile("{v}\n", vars={"v": 1e20}).output == "100000000000000000000\n"


def test_v3_huge_int_rejected() -> None:
    # Python ints are arbitrary-precision; values beyond the u64/f64-exact range
    # are not representable as a JSON number and are rejected. (Documented
    # divergence from napi, where a JS number would already be an f64 — pass a
    # float, e.g. `1e20`, for parity.)
    for huge in (10**20, 10**40):
        with pytest.raises(m.MdsError) as ei:
            m.compile("{v}\n", vars={"v": huge})
        assert ei.value.code == "mds::invalid_options"


def test_v3_nan_inf_become_null() -> None:
    # NaN/Inf are not representable in JSON; they map to null → empty render.
    assert m.compile("{v}\n", vars={"v": math.nan}).output == ""
    assert m.compile("{v}\n", vars={"v": math.inf}).output == ""


def test_v3_none_is_valid_null() -> None:
    # A Python None is a valid null value (renders empty), not an error.
    assert m.compile("{v}\n", vars={"v": None}).output == ""


@pytest.mark.parametrize("bad", [b"bytes", object(), 1 + 2j])
def test_v3_unconvertible_scalar_values(bad: object) -> None:
    # bytes / arbitrary objects / complex can't convert to JSON values →
    # invalid_options. (Sets and tuples DO convert — see below.)
    with pytest.raises(m.MdsError) as ei:
        m.compile("{v}\n", vars={"v": bad})
    assert ei.value.code == "mds::invalid_options"


def test_v3_set_and_tuple_become_arrays() -> None:
    assert m.compile("{v}\n", vars={"v": (1, 2, 3)}).output == "1, 2, 3\n"
    # set ordering is not guaranteed; assert the elements render, comma-joined
    out = m.compile("{v}\n", vars={"v": {1, 2, 3}}).output
    assert sorted(out.strip().split(", ")) == ["1", "2", "3"]


def test_v3_non_string_keys_rejected() -> None:
    with pytest.raises(m.MdsError) as ei:
        m.compile("{v}\n", vars={"v": {1: "x"}})
    assert ei.value.code == "mds::invalid_options"


def test_v3_nested_json_values_accepted() -> None:
    # A nested object var is accepted; access a scalar field (objects cannot be
    # interpolated directly).
    r = m.compile(
        "{cfg.flag} {cfg.items}\n",
        vars={"cfg": {"flag": True, "items": [1, 2], "n": None}},
    )
    assert r.output == "true 1, 2\n"
