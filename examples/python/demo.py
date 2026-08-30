#!/usr/bin/env python3
"""Runnable tour of the ``markdown_script`` Python bindings (PyO3).

Demonstrates the full public API surface:

1. Compiling a template to Markdown (``compile``).
2. Generating a Source Map v3 with ``source_map=True`` and reading it back.
   Also ``compile_file(..., sources_content=True)`` to embed source text.
3. Handling compile errors via ``markdown_script.MdsError`` (``.code`` / ``.help`` / ``.span``).
4. Linting a template with ``lint`` and inspecting the structured findings.
5. Validating without rendering: ``check``, ``check_file``, ``check_virtual``.
   Also MdsError on check failure and the source_map rejection guard.
6. In-memory virtual filesystem: ``compile_virtual`` with a modules dict.
7. Scanning import paths: ``scan_imports`` returns ``@extends`` and ``@import`` paths.
8. File and virtual linting: ``lint_file``, ``lint_virtual``.
9. ``LintResult.lint_warnings`` — unknown rule names produce a v0.4.0 warning instead
   of silently disappearing.
10. Frozen + picklable result classes — ``pickle.loads(pickle.dumps(x))`` round-trips
    and mutation raises ``AttributeError``.

Run it against the repo's virtualenv (see the README):

    source .venv/bin/activate
    python examples/python/demo.py
"""

import json
import os
import pickle

import markdown_script

HERE = os.path.dirname(os.path.abspath(__file__))
# Reuse the two-source example from examples/source-maps/ for the file demo.
ANNOTATED = os.path.join(HERE, "..", "source-maps", "annotated-prompt.mds")
# Fixture file in this directory for check_file and lint_file demos.
SAMPLE_MDS = os.path.join(HERE, "sample.mds")


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


# ── 5. check / check_file / check_virtual ────────────────────────────────────
rule("check / check_file / check_virtual — validate without rendering")

# check: returns CheckResult (warnings list); does not render output
cr = markdown_script.check(
    "# {{greeting}}\n",
    vars={"greeting": "Hello"},
)
print("CheckResult repr:", repr(cr))
print("warnings:", cr.warnings)       # [] — template is valid
print("to_dict:", cr.to_dict())
print("to_json:", cr.to_json())

# check_file: validate a file on disk — no rendering, no source map
cr_file = markdown_script.check_file(SAMPLE_MDS)
print("check_file warnings:", cr_file.warnings)   # [] — file is valid

# check_virtual: validate a module from an in-memory dict
modules_ok = {"m.mds": "# {{title}}\n"}
cr_virt = markdown_script.check_virtual(modules_ok, "m.mds", vars={"title": "Hello"})
print("check_virtual warnings:", cr_virt.warnings)

# check raises MdsError on template errors; .code / .message / .help / .span
# are all present (same attributes as a compile failure — see section 3).
rule("MdsError on check failure")
try:
    markdown_script.check("{{undefined_var}}\n")
except markdown_script.MdsError as exc:
    print("code:", exc.code)
    print("message:", exc.message)
    print("help:", exc.help)
    span = exc.span
    if span is not None:
        print(f"span: offset={span.offset} length={span.length} "
              f"line={span.line} column={span.column}")

# check rejects source_map — source maps are a compile-only concept
try:
    markdown_script.check("# ok\n", source_map=True)
except markdown_script.MdsError as exc:
    print("check() rejects source_map, code:", exc.code)


# ── 6. compile_virtual — in-memory virtual filesystem ────────────────────────
rule("compile_virtual — in-memory virtual filesystem")

# Single-module case: compile from a dict without touching the filesystem
modules = {"prompt.mds": "# {{role}}\n\n{{instructions}}\n"}
vr = markdown_script.compile_virtual(
    modules,
    "prompt.mds",
    vars={"role": "Reviewer", "instructions": "Inspect the code."},
)
print("kind:", vr.kind)
print("output:", vr.output)
print("dependencies:", vr.dependencies)     # [] — no @import in this module

# Multi-module case: @import resolves within the same in-memory dict.
# The entry key is "main.mds"; the import path "./helper.mds" is resolved
# by normalising it against the entry, yielding the key "helper.mds".
helper_src = "@define title():\nVirtual FS Demo\n@end\n"
main_src = '@import "./helper.mds" as h\n# {{h.title()}}\n\n{{body}}\n'
vr2 = markdown_script.compile_virtual(
    {"main.mds": main_src, "helper.mds": helper_src},
    "main.mds",
    vars={"body": "Generated in memory."},
)
print("multi-module output:", vr2.output)
print("multi-module dependencies:", vr2.dependencies)   # ["helper.mds"]


# ── 7. scan_imports — extract @extends and @import paths ────────────────────
rule("scan_imports — extract @extends and @import paths")

# Returns a deduplicated list of import paths in resolution order.
# @extends comes first; @import paths follow in document order.
source_with_imports = (
    '@extends "./base.mds"\n'
    '@import "./utils.mds"\n'
    "# Main template\n"
)
imports = markdown_script.scan_imports(source_with_imports)
print("imports:", imports)   # ['./base.mds', './utils.mds']

# Template with no imports returns []
no_imports = "# Hello {{name}}\n"
print("no imports:", markdown_script.scan_imports(no_imports))

# scan_imports takes its argument positionally (not keyword-only)
annotated_src = open(ANNOTATED).read()
print("annotated-prompt imports:", markdown_script.scan_imports(annotated_src))


# ── 8. lint_file / lint_virtual ──────────────────────────────────────────────
rule("lint_file — lint a file on disk")

# sample.mds has an unused frontmatter key; lint reports it.
lr_file = markdown_script.lint_file(SAMPLE_MDS)
print("lint_file version:", lr_file.version, "truncated:", lr_file.truncated)
for report in lr_file.files:
    print(f"  file: {report.file}")
    for diag in report.diagnostics:
        print(f"  [{diag.severity}] {diag.rule}: {diag.message}")
        if diag.help:
            print(f"    help: {diag.help}")
        if diag.span is not None:
            print(f"    span: offset={diag.span.offset} length={diag.span.length}")

rule("lint_virtual — lint in-memory modules")

# Lint a module defined entirely in memory — same rule set as lint_file.
virt_modules = {
    "check.mds": "---\nname: demo\nstale_key: ignored\n---\n# {{name}}\n",
}
lr_virt = markdown_script.lint_virtual(virt_modules, "check.mds")
print("lint_virtual files:", len(lr_virt.files))
for report in lr_virt.files:
    print(f"  file: {report.file}")
    for diag in report.diagnostics:
        print(f"  [{diag.severity}] {diag.rule}: {diag.message}")

# Override rule severity in virtual lint
lr_virt_err = markdown_script.lint_virtual(
    virt_modules,
    "check.mds",
    rules={"unused-variable": "error"},
)
for report in lr_virt_err.files:
    for diag in report.diagnostics:
        print(f"  severity promoted: {diag.severity}")   # error


# ── 9. lint_warnings — unknown rule names (v0.4.0 behavior) ─────────────────
rule("lint_warnings — unknown rule names (new in v0.4.0)")

# Prior to v0.4.0, an unknown rule name was silently ignored.
# v0.4.0 changed this: unknown rule names are reported in lint_warnings,
# while the call still succeeds and applies all recognised rules normally.
lr_warn = markdown_script.lint("# Hello\n", rules={"nonexistent-rule": "error"})
print("lint_warnings:", lr_warn.lint_warnings)
print("files (0 — unknown rule was not applied):", len(lr_warn.files))

# to_dict() omits the lint_warnings key entirely when there are no warnings
# (Python attribute returns [] in both cases — the empty-default is idiomatic).
lr_clean = markdown_script.lint("# Hello\n")
print("clean to_dict keys:", sorted(lr_clean.to_dict().keys()))    # no lint_warnings
print("clean lint_warnings:", lr_clean.lint_warnings)              # []

# to_dict() includes lint_warnings when warnings are present
print("warn to_dict keys:", sorted(lr_warn.to_dict().keys()))      # includes lint_warnings

# A valid rule name paired with an unknown one: known rules are still enforced
lr_mix = markdown_script.lint(
    "---\nunused: val\n---\n# Hello\n",
    rules={"unused-variable": "error", "bogus-rule": "warn"},
)
print("mix lint_warnings:", lr_mix.lint_warnings)
for report in lr_mix.files:
    for diag in report.diagnostics:
        # unused-variable was applied (severity elevated to error); bogus-rule was not
        print(f"  [{diag.severity}] {diag.rule}")


# ── 10. Frozen + picklable result classes ────────────────────────────────────
rule("frozen + picklable result classes")

# All result classes are #[pyclass(frozen)] — mutation raises AttributeError.
r = markdown_script.compile("# {{name}}\n", vars={"name": "World"})
try:
    r.output = "mutated"  # type: ignore[misc]
except AttributeError as exc:
    print("frozen CompileResult raises AttributeError:", type(exc).__name__)

# pickle.dumps / pickle.loads round-trips preserve value equality.
r_pickled = pickle.loads(pickle.dumps(r))
print("CompileResult round-trip equal:", r == r_pickled)
print("pickled output:", r_pickled.output)

# CheckResult pickle round-trip
cr = markdown_script.check("# {{greeting}}\n", vars={"greeting": "Hello"})
cr_pickled = pickle.loads(pickle.dumps(cr))
print("CheckResult round-trip equal:", cr == cr_pickled)
print("pickled warnings:", cr_pickled.warnings)

# LintResult pickle round-trip (includes nested LintFileReport + LintDiagnostic)
lr = markdown_script.lint("---\nunused: 1\nused: hi\n---\n{{used}}\n")
lr_pickled = pickle.loads(pickle.dumps(lr))
print("LintResult round-trip equal:", lr == lr_pickled)
print("LintResult pickled files count:", len(lr_pickled.files))
pickled_report = lr_pickled.files[0]
print("LintFileReport file:", pickled_report.file)
for diag in pickled_report.diagnostics:
    print(f"  pickled diagnostic: {diag.rule} [{diag.severity}]")

# Span is also frozen and picklable
span = markdown_script.Span(offset=10, length=5, line=2, column=3)
span_pickled = pickle.loads(pickle.dumps(span))
print("Span round-trip equal:", span == span_pickled)
print("Span offset:", span_pickled.offset, "line:", span_pickled.line)
