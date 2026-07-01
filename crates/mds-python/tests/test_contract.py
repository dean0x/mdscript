"""Result shape, invariants, and frozen/eq semantics (AC-C2..C5)."""

from __future__ import annotations

import json

import pytest

import mdscript as m

MD = m.compile("Hello {name}!\n", vars={"name": "Alice"})
MSG = m.compile("@message user:\nHi\n@end\n")


# ── C2: CompileResult members + frozen ──────────────────────────────────────────


def test_c2_compile_result_members_markdown() -> None:
    assert MD.kind == "markdown"
    assert isinstance(MD.output, str)
    assert MD.messages is None
    assert isinstance(MD.warnings, list)
    assert isinstance(MD.dependencies, list)


def test_c2_compile_result_members_messages() -> None:
    assert MSG.kind == "messages"
    assert MSG.output is None
    assert isinstance(MSG.messages, list)
    assert all(isinstance(x, m.Message) for x in MSG.messages)


def test_c2_frozen_getter_assignment_raises() -> None:
    with pytest.raises(AttributeError):
        MD.kind = "messages"  # type: ignore[misc]
    with pytest.raises(AttributeError):
        MD.output = "x"  # type: ignore[misc]


def test_c2_frozen_no_new_attributes() -> None:
    with pytest.raises(AttributeError):
        MD.bogus = 1  # type: ignore[attr-defined]


# ── C3: Message / CheckResult / Span typed ──────────────────────────────────────


def test_c3_message_typed() -> None:
    msg = MSG.messages[0]  # type: ignore[index]
    assert isinstance(msg.role, str) and msg.role == "user"
    assert isinstance(msg.content, str)
    with pytest.raises(AttributeError):
        msg.role = "system"  # type: ignore[misc]


def test_c3_check_result_typed() -> None:
    cr = m.check("Hi\n")
    assert isinstance(cr.warnings, list)
    with pytest.raises(AttributeError):
        cr.warnings = []  # type: ignore[misc]


def test_c3_span_typed() -> None:
    sp = m.Span(3, 5, 1, 4)
    assert (sp.offset, sp.length, sp.line, sp.column) == (3, 5, 1, 4)
    sp2 = m.Span(0, 1)  # line/column default to None
    assert sp2.line is None and sp2.column is None


# ── C4: to_dict()/to_json() byte-identical to canonical; inactive key absent ─────


def test_c4_markdown_to_dict_canonical() -> None:
    assert MD.to_dict() == {
        "kind": "markdown",
        "output": "Hello Alice!\n",
        "warnings": [],
        "dependencies": [],
    }
    assert "messages" not in MD.to_dict()


def test_c4_messages_to_dict_canonical() -> None:
    d = MSG.to_dict()
    assert d["kind"] == "messages"
    assert "output" not in d
    assert d["messages"] == [{"role": "user", "content": "Hi"}]  # content is trimmed


def test_c4_to_json_matches_to_dict() -> None:
    for r in (MD, MSG):
        assert json.loads(r.to_json()) == r.to_dict()


def test_c4_check_to_dict_json() -> None:
    cr = m.check("Hi\n")
    assert cr.to_dict() == {"warnings": []}
    assert json.loads(cr.to_json()) == {"warnings": []}


def test_c4_span_message_to_dict_json() -> None:
    sp = m.Span(3, 5, 1, 4)
    assert sp.to_dict() == {"offset": 3, "length": 5, "line": 1, "column": 4}
    assert json.loads(sp.to_json()) == sp.to_dict()
    msg = m.Message("user", "hi")
    assert msg.to_dict() == {"role": "user", "content": "hi"}
    assert json.loads(msg.to_json()) == msg.to_dict()


# ── C5: single backing store invariant ──────────────────────────────────────────


def test_c5_single_backing_store() -> None:
    # Every typed getter reads from the same canonical value that to_dict() returns.
    d = MD.to_dict()
    assert MD.kind == d["kind"]
    assert MD.output == d["output"]
    assert MD.warnings == d["warnings"]
    assert MD.dependencies == d["dependencies"]
    dm = MSG.to_dict()
    assert [{"role": x.role, "content": x.content} for x in MSG.messages] == dm[  # type: ignore[union-attr]
        "messages"
    ]


def test_c5_to_dict_returns_independent_copies() -> None:
    # Mutating a returned dict must not corrupt the frozen backing store.
    d = MD.to_dict()
    d["warnings"].append("mutated")
    assert MD.warnings == []


# ── repr / eq ───────────────────────────────────────────────────────────────────


def test_repr_is_informative() -> None:
    assert "CompileResult(kind='markdown'" in repr(MD)
    assert "CompileResult(kind='messages'" in repr(MSG)
    assert repr(m.Span(1, 2, 3, 4)) == "Span(offset=1, length=2, line=3, column=4)"
    assert repr(m.Span(1, 2)) == "Span(offset=1, length=2, line=None, column=None)"


def test_eq_is_wire_equality() -> None:
    assert m.compile("Hi\n") == m.compile("Hi\n")
    assert m.compile("Hi\n") != m.compile("Bye\n")
    assert m.compile("Hi\n") != m.check("Hi\n")  # different types
    assert m.compile("Hi\n") != "Hi\n"  # non-result compares unequal, no error
    assert m.Span(1, 2, 3, 4) == m.Span(1, 2, 3, 4)
    assert m.Message("u", "c") == m.Message("u", "c")
    assert m.Message("u", "c") != m.Message("u", "d")


def test_unhashable() -> None:
    for obj in (MD, MSG, m.check("Hi\n"), m.Span(1, 2, 3, 4), m.Message("u", "c")):
        with pytest.raises(TypeError):
            hash(obj)
