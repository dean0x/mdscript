#!/usr/bin/env python3
"""Runnable tour of the ``markdown_script`` Python bindings (PyO3).

Demonstrates four things a real user needs:

1. Compiling a template to Markdown.
2. Generating a Source Map v3 with ``source_map=True`` and reading it back.
3. Handling compile errors via ``markdown_script.MdsError`` (``.code`` / ``.help`` / ``.span``).
4. Linting a template and inspecting the structured findings.

Run it against the repo's virtualenv (see the README):

    source .venv/bin/activate
    python examples/python/demo.py
"""

import json
import os

import markdown_script

HERE = os.path.dirname(os.path.abspath(__file__))
# Reuse the two-source example from examples/source-maps/ for the file demo.
ANNOTATED = os.path.join(HERE, "..", "source-maps", "annotated-prompt.mds")


def rule(title: str) -> None:
    print(f"\n{'=' * 3} {title} {'=' * 3}")


# ── 1. Compile a string to Markdown ─────────────────────────────────────────
rule("compile a string")
result = markdown_script.compile(
    "# Hi {{name}}\n\n@for item in items:\n- {{item}}\n@end\n",
    vars={"name": "World", "items": ["alpha", "beta"]},
)
print("kind:", result.kind)
print("output:\n" + result.output)


# ── 2. Compile with a Source Map v3 ─────────────────────────────────────────
rule("compile a string with source_map=True")
mapped = markdown_script.compile(
    "# Hi {{name}}\n\n@for item in items:\n- {{item}}\n@end\n",
    vars={"name": "World", "items": ["alpha", "beta"]},
    source_map=True,
)
# ``.source_map`` is a plain dict (None when source_map was not requested).
sm = mapped.source_map
print("source_map is a dict:", isinstance(sm, dict))
print("version:", sm["version"])
print("sources:", sm["sources"])  # string compiles report ["input.mds"]
print("has 'file' key:", "file" in sm)  # bindings omit it (CLI-only field)
print("mappings:", sm["mappings"])
# When not requested, the attribute is None.
# to_dict() always includes "sourceMap": None (Python-idiomatic always-present);
# to_json() omits the key (canonical wire format shared with other surfaces).
plain = markdown_script.compile("# no map\n")
print("without source_map -> .source_map is None:", plain.source_map is None)
d = plain.to_dict()
print("to_dict has 'sourceMap' key:", "sourceMap" in d)
print("to_dict['sourceMap'] is None:", d["sourceMap"] is None)

# File compile resolves @import chains; sources become project-root-relative
# (or basenames when no .git/.mdsroot marker is found above the file).
rule("compile_file with an @import + embedded sources")
filemap = markdown_script.compile_file(ANNOTATED, source_map=True, sources_content=True)
fsm = filemap.source_map
print("sources:", fsm["sources"])  # entry template + imported _style.mds
print("sourcesContent lengths:", [len(c) for c in fsm["sourcesContent"]])
print("dependencies:", [os.path.basename(d) for d in filemap.dependencies])

# Requesting sources_content without source_map is rejected.
rule("sources_content without source_map is an error")
try:
    markdown_script.compile("x\n", sources_content=True)
except markdown_script.MdsError as exc:
    print("raised MdsError, code:", exc.code)


# ── 3. Error handling via MdsError ──────────────────────────────────────────
rule("error handling")
try:
    markdown_script.compile("Hello {{missing}}!\n")
except markdown_script.MdsError as exc:
    print("code:", exc.code)
    print("help:", exc.help)
    span = exc.span
    if span is not None:
        print(f"span: offset={span.offset} length={span.length} "
              f"line={span.line} column={span.column}")


# ── 4. Lint a template ──────────────────────────────────────────────────────
rule("lint")
lint_result = markdown_script.lint("---\nunused: 1\nused: hi\n---\n{{used}}\n")
print("lint schema version:", lint_result.version, "truncated:", lint_result.truncated)
# LintResult.files returns a list of LintFileReport objects (B6/F10 typed access).
for report in lint_result.files:
    print(f"  file: {report.file}")
    for diag in report.diagnostics:
        print(f"  [{diag.severity}] {diag.rule}: {diag.message}")
        if diag.help:
            print(f"    help: {diag.help}")
        if diag.span is not None:
            print(f"    span: offset={diag.span.offset} length={diag.span.length}")

# Every result object also serializes to a dict / JSON string.
print("\nlint as JSON:")
print(json.dumps(json.loads(lint_result.to_json()), indent=2))
