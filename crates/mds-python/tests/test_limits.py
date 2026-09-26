"""Boundary guards and vars value edges (AC-L*, AC-V3)."""

from __future__ import annotations

import math

import pytest

import markdown_script as m

MAX = 10 * 1024 * 1024  # MAX_SOURCE_SIZE (10 MiB)
FM_MAX = 1 << 20  # MAX_FRONTMATTER_SIZE (1 MiB)


# ── Frontmatter YAML DoS bomb builders (#162) ────────────────────────────────────
# Built by string repetition so there is no MiB-scale literal in the test source.


def wrap_fm(yaml: str) -> str:
    return f"---\n{yaml}---\nHi\n"


def alias_bomb(n: int, mm: int) -> str:
    # a: &a [x, x, ...(n)]  /  b: [*a, *a, ...(mm)] — each *a re-expands the anchor.
    return "a: &a [" + "x, " * n + "]\nb: [" + "*a, " * mm + "]\n"


def fm_of_size(nbytes: int) -> str:
    prefix = "k: ZZSENTINELZZ"  # sentinel proves the message never echoes content
    return prefix + "x" * (nbytes - len(prefix) - 1) + "\n"


def nested_flow_seq(d: int) -> str:
    return "k: " + "[" * d + "x" + "]" * d + "\n"


# The sub-1 MiB memory-amplification repro: ~700 KB source (under the size cap), so the
# node budget is what rejects it.
BOMB_DOC = wrap_fm(alias_bomb(100_000, 100_000))


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


def test_l2_check_virtual_too_many_modules() -> None:
    # Boundary guards are shared via parse_modules, not forked per function —
    # check_virtual must enforce the same module-count cap as compile_virtual.
    mods = {f"m{i}.mds": "x" for i in range(257)}
    with pytest.raises(m.MdsError) as ei:
        m.check_virtual(mods, "m0.mds")
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


def test_l2_bad_value_module_key_is_escaped_in_message() -> None:
    # #265: the module key a message names is escaped — every forbidden path
    # character, TAB included — as the same six-character text the other bindings use.
    for cp in (0x09, 0x1B, 0x0A, 0x202E):
        key = "x" + chr(cp) + ".mds"
        shown = "x" + "\\u" + format(cp, "04X") + ".mds"
        for call in (m.compile_virtual, m.lint_virtual):
            with pytest.raises(m.MdsError) as ei:
                call({key: 5}, "main.mds")  # type: ignore[dict-item]
            assert ei.value.code == "mds::invalid_options"
            assert ei.value.message == f'modules["{shown}"] must be a string, got number'
            assert chr(cp) not in ei.value.message
    # Control: a clean key is shown as written.
    with pytest.raises(m.MdsError) as ei:
        m.compile_virtual({"ok.mds": 5}, "main.mds")  # type: ignore[dict-item]
    assert ei.value.message == 'modules["ok.mds"] must be a string, got number'


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
        m.compile("{{v}}\n", vars={"v": nested})
    assert ei.value.code in ("mds::invalid_options", "mds::json")


# ── V3: vars value edges — defined outcomes + numeric parity vs napi ─────────────


def test_v3_plain_numbers() -> None:
    assert m.compile("{{v}}\n", vars={"v": 42}).output == "42\n"
    assert m.compile("{{v}}\n", vars={"v": 1.5}).output == "1.5\n"


def test_v3_float_1e20_parity_with_napi() -> None:
    # JS numbers are f64; napi renders `1e20` as an integer string. A Python float
    # goes through the same f64 path, so parity holds exactly.
    assert m.compile("{{v}}\n", vars={"v": 1e20}).output == "100000000000000000000\n"


def test_v3_huge_int_rejected() -> None:
    # Python ints are arbitrary-precision; values beyond the u64/f64-exact range
    # are not representable as a JSON number and are rejected. (Documented
    # divergence from napi, where a JS number would already be an f64 — pass a
    # float, e.g. `1e20`, for parity.)
    for huge in (10**20, 10**40):
        with pytest.raises(m.MdsError) as ei:
            m.compile("{{v}}\n", vars={"v": huge})
        assert ei.value.code == "mds::invalid_options"


def test_v3_nan_inf_become_null() -> None:
    # NaN/Inf are not representable in JSON; they map to null → empty render.
    assert m.compile("{{v}}\n", vars={"v": math.nan}).output == ""
    assert m.compile("{{v}}\n", vars={"v": math.inf}).output == ""


def test_v3_none_is_valid_null() -> None:
    # A Python None is a valid null value (renders empty), not an error.
    assert m.compile("{{v}}\n", vars={"v": None}).output == ""


@pytest.mark.parametrize("bad", [b"bytes", object(), 1 + 2j])
def test_v3_unconvertible_scalar_values(bad: object) -> None:
    # bytes / arbitrary objects / complex can't convert to JSON values →
    # invalid_options. (Sets and tuples DO convert — see below.)
    with pytest.raises(m.MdsError) as ei:
        m.compile("{{v}}\n", vars={"v": bad})
    assert ei.value.code == "mds::invalid_options"


def test_v3_set_and_tuple_become_arrays() -> None:
    assert m.compile("{{v}}\n", vars={"v": (1, 2, 3)}).output == "1, 2, 3\n"
    # set ordering is not guaranteed; assert the elements render, comma-joined
    out = m.compile("{{v}}\n", vars={"v": {1, 2, 3}}).output
    assert sorted(out.strip().split(", ")) == ["1", "2", "3"]


def test_v3_non_string_keys_rejected() -> None:
    with pytest.raises(m.MdsError) as ei:
        m.compile("{{v}}\n", vars={"v": {1: "x"}})
    assert ei.value.code == "mds::invalid_options"


def test_v3_nested_json_values_accepted() -> None:
    # A nested object var is accepted; access a scalar field (objects cannot be
    # interpolated directly).
    r = m.compile(
        "{{cfg.flag}} {{cfg.items}}\n",
        vars={"cfg": {"flag": True, "items": [1, 2], "n": None}},
    )
    assert r.output == "true 1, 2\n"


# ── L4: frontmatter YAML DoS bounds across every entry point (#162) ──────────────


@pytest.mark.parametrize(
    "fn",
    [m.compile, m.check, m.lint, m.scan_imports],
    ids=["compile", "check", "lint", "scan_imports"],
)
def test_l4_alias_bomb_is_resource_limit(fn) -> None:  # type: ignore[no-untyped-def]
    # The bomb rejects with a resource limit on every surface, and the message never
    # echoes the raw hostile bytes (`*a` or the sentinel).
    with pytest.raises(m.MdsError) as ei:
        fn(BOMB_DOC)
    assert ei.value.code == "mds::resource_limit"
    assert "*a" not in ei.value.message
    assert "ZZSENTINELZZ" not in ei.value.message


def test_l4_frontmatter_over_size_cap_is_resource_limit() -> None:
    with pytest.raises(m.MdsError) as ei:
        m.compile(wrap_fm(fm_of_size(FM_MAX + 1)))
    assert ei.value.code == "mds::resource_limit"
    assert "ZZSENTINELZZ" not in ei.value.message


def test_l4_frontmatter_at_size_cap_is_accepted() -> None:
    # At-cap control (PF-013): exactly 1 MiB of frontmatter compiles.
    r = m.compile(wrap_fm(fm_of_size(FM_MAX)))
    assert isinstance(r.output, str)


def test_l4_legit_anchor_alias_still_compiles() -> None:
    # Positive control (PF-013): valid YAML aliasing must not be over-rejected.
    r = m.compile("---\na: &a [1, 2]\nb: *a\n---\n{{b}}\n")
    assert r.output.endswith("1, 2\n")


def test_l4_scan_imports_stays_lenient_for_frontmatter_syntax_errors() -> None:
    # scan_imports swallows a plain frontmatter YAML SYNTAX error and still returns the
    # body imports — proving the resource-limit propagation above is specific to the
    # bound, not blanket strictness. (Bomb propagation is covered by the parametrised
    # scan_imports case.)
    src = '---\nimports: [\n---\n@import "./x.mds"\nHi\n'
    assert m.scan_imports(src) == ["./x.mds"]


def test_l4_deep_flow_nest_is_resource_limit() -> None:
    # The second DoS axis: a deep flow nest (depth 2000 > 1024) trips the pre-parse
    # flow-depth guard.
    with pytest.raises(m.MdsError) as ei:
        m.compile(wrap_fm(nested_flow_seq(2000)))
    assert ei.value.code == "mds::resource_limit"
