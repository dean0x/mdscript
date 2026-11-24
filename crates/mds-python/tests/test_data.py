"""Local data sources in the Python binding (#405).

An entry binds a local ``.json``, ``.yaml`` or ``.yml`` file through ``compile``,
``compile_file`` and ``compile_virtual``; the check and lint calls read no data file;
``dependencies`` lists modules, then data files; a ``data`` header never reaches the
output; and a virtual map counts modules and data files apart.
"""

from __future__ import annotations

import os
import pathlib

import pytest

import markdown_script as m
from conftest import cli_build

MIB = 1024 * 1024

# The fixture of the napi, WASM and CLI data tests: a .json, a .yaml and a .yml entry,
# each bound under its name.
DATA_FIXTURE = (
    "---\ndata:\n  cfg: ./cfg.json\n  list: ./list.yaml\n  more: ./more.yml\n---\n"
    "{{cfg.name}}: {{list.count}} {{more.unit}}\n"
)
DATA_FILES = {
    "cfg.json": '{"name": "Ada"}\n',
    "list.yaml": "count: 3\n",
    "more.yml": "unit: items\n",
}
RENDERED = "Ada: 3 items\n"

# Text that no data parser accepts: a read of it fails with mds::yaml.
BAD_DATA = "["

MARKER = "./fixture-marker.yml"
FORM_MESSAGE = "YAML parse error: write `data:` at the start of a line"

# The bounds of a virtual map, and their messages: equal on WASM, napi and Python.
DATA_COUNT = "modules exceeds maximum data file count of 256 (257 provided)"
MODULE_COUNT = "modules exceeds maximum module count of 257 (258 provided)"
DATA_BYTES = "modules data aggregate size exceeds maximum of 33554432 bytes"
MODULE_BYTES = "modules aggregate size exceeds maximum of 10485760 bytes"

VIRTUAL_CALLS = pytest.mark.parametrize(
    "fn",
    [m.compile_virtual, m.check_virtual, m.lint_virtual],
    ids=["compile", "check", "lint"],
)


def project(root: pathlib.Path, files: dict[str, str]) -> pathlib.Path:
    """Write ``files`` below ``root`` with a ``.mdsroot`` marker, and return ``root``."""
    (root / ".mdsroot").write_bytes(b"")
    for name, text in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        # write_bytes keeps LF line ends on every OS, so the bytes equal the virtual text.
        path.write_bytes(text.encode("utf-8"))
    return root


def same_files(paths: list[str], root: pathlib.Path, names: list[str]) -> bool:
    """Whether ``paths``, in order, are the files ``root / name`` of ``names``."""
    return len(paths) == len(names) and all(
        os.path.isabs(path) and os.path.samefile(path, root / name)
        for path, name in zip(paths, names)
    )


def raises(call: object, *args: object, **kwargs: object) -> m.MdsError:
    """Call ``call`` and return the :class:`MdsError` it raises."""
    with pytest.raises(m.MdsError) as ei:
        call(*args, **kwargs)  # type: ignore[operator]
    return ei.value


# ── AC-8: an entry binds a local file on every compile call ──────────────────────


def test_d1_compile_calls_bind_json_and_yaml_entries(tmp_path: pathlib.Path) -> None:
    root = project(tmp_path, {"main.mds": DATA_FIXTURE, **DATA_FILES})
    from_string = m.compile(DATA_FIXTURE, base_path=root)
    from_file = m.compile_file(root / "main.mds")
    from_map = m.compile_virtual({"main.mds": DATA_FIXTURE, **DATA_FILES}, "main.mds")
    names = ["cfg.json", "list.yaml", "more.yml"]
    for result in (from_string, from_file, from_map):
        assert result.output == RENDERED
    assert same_files(from_string.dependencies, root, names)
    assert same_files(from_file.dependencies, root, names)
    assert from_map.dependencies == names


def test_d2_compile_output_equals_the_cli_build(
    mds_cli: pathlib.Path, tmp_path: pathlib.Path
) -> None:
    # Compare the two surfaces with each other, not each with its own expected text.
    root = project(tmp_path, DATA_FILES)
    assert m.compile(DATA_FIXTURE, base_path=root).output == cli_build(
        mds_cli, DATA_FIXTURE, root
    )
    title = "---\ndata:\n  title: ./title.json\n---\nTitle: {{title}}\n"
    project(tmp_path, {"title.json": BAD_DATA})
    python = m.compile(title, base_path=root, vars={"title": "Replayed"}).output
    assert python == "Title: Replayed\n"
    assert python == cli_build(mds_cli, title, root, "--set-string", "title=Replayed")


# ── AC-16 and AC-6: replay, and a runtime `data` variable ────────────────────────


def test_d3_runtime_vars_replay_an_entry_and_read_no_file(tmp_path: pathlib.Path) -> None:
    source = "---\ndata:\n  title: ./title.json\n---\nTitle: {{title}}\n"
    root = project(tmp_path, {"title.json": BAD_DATA})
    modules = {"main.mds": source, "title.json": BAD_DATA}
    replay = {"title": "Replayed"}
    for result in (
        m.compile(source, base_path=root, vars=replay),
        m.compile_virtual(modules, "main.mds", vars=replay),
    ):
        assert result.output == "Title: Replayed\n"
        assert result.dependencies == []
    # Control: with no replay, the compile reads the file and fails.
    for err in (
        raises(m.compile, source, base_path=root),
        raises(m.compile_virtual, modules, "main.mds"),
    ):
        assert err.code == "mds::yaml"
        assert 'data file "title.json", entry "title"' in err.message


def test_d4_a_runtime_data_variable_defines_no_entry(tmp_path: pathlib.Path) -> None:
    root = project(tmp_path, {"bad.json": BAD_DATA})
    runtime = {"data": {"cfg": "./bad.json"}}
    result = m.compile("{{data.cfg}}\n", base_path=root, vars=runtime)
    assert result.output == "./bad.json\n"
    assert result.dependencies == []
    # Control: the same entry in frontmatter reads the file and fails with mds::yaml.
    err = raises(m.compile, "---\ndata:\n  cfg: ./bad.json\n---\n{{cfg}}\n", base_path=root)
    assert err.code == "mds::yaml"


# ── AC-19: the check and lint calls read no data file ────────────────────────────

PENDING = (
    "---\ndata:\n  rows: ./bad.yml\n  other: ./absent.json\n---\n"
    "@for row in rows:\n{{row.name}}\n@end\n{{other}}\n"
)
PENDING_FILES = {"main.mds": PENDING, "bad.yml": BAD_DATA}


def test_d5_check_calls_read_no_data_file_and_warn_once(tmp_path: pathlib.Path) -> None:
    # bad.yml does not parse and absent.json does not exist: a read of either fails.
    root = project(tmp_path, PENDING_FILES)
    for result in (
        m.check(PENDING, base_path=root),
        m.check_file(root / "main.mds"),
        m.check_virtual(PENDING_FILES, "main.mds"),
    ):
        assert len(result.warnings) == 1
        warning = result.warnings[0]
        assert warning.startswith("data pending; output was not evaluated: ")
        assert 'entry "rows"' in warning and 'entry "other"' in warning
    # Control: a misspelled name that is not an entry still fails the check.
    typo = PENDING.replace("{{other}}", "{{typo}}")
    assert raises(m.check, typo, base_path=root).code == "mds::undefined_var"


def test_d6_lint_calls_read_no_data_file(tmp_path: pathlib.Path) -> None:
    root = project(tmp_path, PENDING_FILES)
    for result in (
        m.lint(PENDING, base_path=root),
        m.lint_file(root / "main.mds"),
        m.lint_virtual(PENDING_FILES, "main.mds"),
    ):
        assert [d.to_dict() for f in result.files for d in f.diagnostics] == []


def test_d7_compile_calls_read_the_data_file_that_check_skips(
    tmp_path: pathlib.Path,
) -> None:
    # Control for d5 and d6: the compile calls read bad.yml and fail with mds::yaml.
    root = project(tmp_path, PENDING_FILES)
    for err in (
        raises(m.compile, PENDING, base_path=root),
        raises(m.compile_file, root / "main.mds"),
        raises(m.compile_virtual, PENDING_FILES, "main.mds"),
    ):
        assert err.code == "mds::yaml"
        assert 'data file "bad.yml", entry "rows"' in err.message


# ── AC-22: dependencies, and scan_imports ────────────────────────────────────────


def test_d8_dependencies_list_modules_then_data_files_in_first_read_order(
    tmp_path: pathlib.Path,
) -> None:
    # The entry binds its entries before it resolves its import. A file that two
    # modules name is listed once, and the entry is not listed.
    main = (
        "---\ndata:\n  shared: ./shared.json\n  own: ./own.yml\n---\n"
        '@import "./a.mds" as a\n{{shared}} {{own}}\n@include a\n'
    )
    a = "---\ndata:\n  item: ./parts/a.json\n  same: ./shared.json\n---\n{{item}} {{same}}\n"
    files = {
        "main.mds": main,
        "a.mds": a,
        "shared.json": "1",
        "own.yml": "2",
        "parts/a.json": "3",
    }
    expected = ["a.mds", "shared.json", "own.yml", "parts/a.json"]
    virtual = m.compile_virtual(files, "main.mds")
    assert virtual.output == "1 2\n3 1\n"
    assert virtual.dependencies == expected
    root = project(tmp_path, files)
    native = m.compile_file(root / "main.mds")
    assert native.output == virtual.output
    assert same_files(native.dependencies, root, expected)


def test_d9_scan_imports_lists_modules_only() -> None:
    source = (
        "---\ndata:\n  cfg: ./cfg.json\n  list: ./list.yaml\n"
        "imports: [{path: ./lib.mds, as: lib}]\n---\n"
        '@import "./b.mds" as b\n{{cfg}}\n'
    )
    # The frontmatter is read (its import is listed), and its data paths are not.
    assert m.scan_imports(source) == ["./lib.mds", "./b.mds"]


# ── AC-10: a data header never reaches the output ────────────────────────────────


@pytest.mark.parametrize(
    "header, kept",
    [
        (f"data:\n  local: {MARKER}", ""),
        (f"data:\n# Data comment\n  local: {MARKER}", "# Data comment\n"),
        (f"data: {{local: {MARKER}}}", ""),
    ],
    ids=["block", "block-comment", "flow"],
)
def test_d10_a_data_header_is_absent_from_output(
    tmp_path: pathlib.Path, header: str, kept: str
) -> None:
    info = f"info: {MARKER}"
    source = f"---\n{header}\n{info}\n---\n{{{{local}}}}\n"
    root = project(tmp_path, {"fixture-marker.yml": "Local"})
    modules = {"main.mds": source, "fixture-marker.yml": "Local"}
    # Control: the marker under the key `info` stays.
    expected = f"---\n{kept}{info}\n---\nLocal\n"
    assert m.compile(source, base_path=root).output == expected
    assert m.compile_virtual(modules, "main.mds").output == expected


def test_d11_a_quoted_or_flow_data_key_fails_with_the_form_message() -> None:
    modules = {"fixture-marker.yml": "Local"}
    # Control: a quoted `info` key compiles and stays.
    control = f"---\n'info': {{local: {MARKER}}}\n---\nBody\n"
    assert m.compile_virtual({**modules, "main.mds": control}, "main.mds").output == control
    for header in (f"'data': {{local: {MARKER}}}", f"{{data: {{local: {MARKER}}}}}"):
        source = f"---\n{header}\n---\nBody\n"
        err = raises(m.compile_virtual, {**modules, "main.mds": source}, "main.mds")
        assert (err.code, err.message) == ("mds::yaml", FORM_MESSAGE)


def test_d12_extends_output_merges_info_and_omits_both_data_headers() -> None:
    child = (
        f"---\ndata: {{local: {MARKER}}}\ninfo: {{child: kept}}\n---\n"
        '@extends "./base.mds"\n'
    )
    base = (
        f"---\ndata: {{base: {MARKER}}}\ninfo: {{base: kept}}\n---\n"
        "@block body:\n{{base}} {{local}}\n@end\n"
    )
    modules = {"main.mds": child, "base.mds": base, "fixture-marker.yml": "Local"}
    output = m.compile_virtual(modules, "main.mds").output or ""
    # Control: the `info` keys of both files are merged into the output.
    assert "info:\n  base: kept\n  child: kept\n" in output
    assert output.endswith("Local Local\n")
    assert "data:" not in output and MARKER not in output


# ── AC-24: data messages name the module, entry and field, escaped ────────────────


@pytest.mark.parametrize("cp", [0x1B, 0x202E], ids=["esc", "rlo"])
def test_d13_a_data_message_escapes_a_hostile_key(cp: int) -> None:
    # The JSON file text holds the escape, so the key decodes to the code point.
    escaped = chr(92) + "u" + format(cp, "04X")
    source = "---\ndata:\n  settings: ./s.json\n---\n{{settings}}\n"
    raw = '{"outer": {"key' + escaped + '": 0}}'
    err = raises(m.compile_virtual, {"main.mds": source, "s.json": raw}, "main.mds")
    assert err.code == "mds::data"
    assert f'"main.mds", entry "settings", field "outer.key{escaped}"' in err.message
    assert chr(cp) not in err.message
    # Control: the same file with a plain key compiles.
    control = source.replace("{{settings}}", "{{settings.outer.key}}")
    modules = {"main.mds": control, "s.json": '{"outer": {"key": 0}}'}
    assert m.compile_virtual(modules, "main.mds").output == "0\n"


def test_d14_a_data_message_escapes_a_hostile_entry_name() -> None:
    # YAML decodes `\e` in a double-quoted key to U+001B.
    source = '---\ndata:\n  "a\\eb": ./s.json\n---\nBody\n'
    err = raises(m.compile_virtual, {"main.mds": source, "s.json": "1"}, "main.mds")
    assert err.code == "mds::data"
    assert 'entry "a' + chr(92) + "u001Bb" + '"' in err.message
    assert chr(0x1B) not in err.message


# ── AC-23: a virtual map counts modules and data files apart ─────────────────────


def map_of(modules: int, data: int) -> dict[str, str]:
    """A map of ``modules`` module keys (``m0.mds`` is the entry) and ``data`` data keys."""
    result = {f"m{i}.mds": "Body\n" for i in range(modules)}
    result["m0.mds"] = "OK\n"
    for i in range(data):
        result[f"d{i}.{('json', 'yaml', 'yml')[i % 3]}"] = "1"
    return result


def assert_refused(fn: object, modules: object, message: str) -> None:
    err = raises(fn, modules, "m0.mds")
    assert (err.code, err.message) == ("mds::resource_limit", message)


@VIRTUAL_CALLS
def test_d15_map_holds_the_entry_256_modules_and_256_data_files(fn: object) -> None:
    full = map_of(257, 256)
    result = fn(full, "m0.mds")  # type: ignore[operator]
    if fn is m.compile_virtual:
        assert result.output == "OK\n"
    assert_refused(fn, {**full, "d256.json": "1"}, DATA_COUNT)
    assert_refused(fn, {**full, "unused.mds": "Body\n"}, MODULE_COUNT)
    # The extension is compared exactly: a .JSON key is a module key.
    assert_refused(fn, {**full, "D.JSON": "1"}, MODULE_COUNT)


@VIRTUAL_CALLS
def test_d16_a_small_map_counts_its_data_files_after_conversion(fn: object) -> None:
    # 258 keys: below the dict fast path, so the count after conversion refuses it.
    fn(map_of(1, 256), "m0.mds")  # type: ignore[operator]
    assert_refused(fn, map_of(1, 257), DATA_COUNT)


@VIRTUAL_CALLS
def test_d17_a_large_dict_is_refused_before_any_value_is_converted(fn: object) -> None:
    # A value that the conversion refuses. Past 513 keys, the count message comes first.
    bad = object()
    past_data = {**map_of(257, 257), "m1.mds": bad}
    assert_refused(fn, past_data, DATA_COUNT)
    # A key that is not a str counts as a module key.
    past_modules = {**map_of(257, 256), 1: bad}
    assert_refused(fn, past_modules, MODULE_COUNT)
    # Control: at 513 keys the conversion runs, and refuses the value.
    at_cap = {**map_of(257, 256), "m1.mds": bad}
    err = raises(fn, at_cap, "m0.mds")
    assert err.code == "mds::invalid_options"


@VIRTUAL_CALLS
def test_d18_map_bounds_module_and_data_text_apart(fn: object) -> None:
    modules = {"m0.mds": "OK\n", "big.mds": "x" * (10 * MIB - 3)}
    for i in range(4):
        modules[f"d{i}.json"] = "x" * (8 * MIB)
    fn(modules, "m0.mds")  # type: ignore[operator]
    assert_refused(fn, {**modules, "d0.json": "x" * (8 * MIB + 1)}, DATA_BYTES)
    assert_refused(fn, {**modules, "big.mds": "x" * (10 * MIB - 2)}, MODULE_BYTES)
