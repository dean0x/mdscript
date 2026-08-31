# Python bindings

`markdown_script` is the native Python binding for the MDS compiler (built with PyO3).
It exposes the same compiler as the CLI and the JavaScript packages, including
**Source Map v3** generation and a full lint API.

## Setup

The bindings are installed into the repo's virtualenv with
[maturin](https://www.maturin.rs/). From the repository root:

```bash
python -m venv .venv
source .venv/bin/activate
pip install "maturin==1.13.3" pytest mypy pyright
maturin develop -m crates/mds-python/Cargo.toml
```

Once the venv is active, `import markdown_script` works.

If a `.venv` already exists (as it does in a normal dev checkout), activate it and
re-run `maturin develop` to pick up any Rust changes; no need to recreate the venv.

## Run the demo

```bash
source .venv/bin/activate
python examples/python/demo.py
```

`demo.py` covers the complete public API surface in ten numbered sections:

1. **Compile a string** to Markdown with runtime `vars`.
2. **Generate a source map** with `source_map=True` and read it back — plus
   `compile_file(..., sources_content=True)` to embed the original template text.
3. **Handle compile errors** — a failed compile raises `markdown_script.MdsError`, which
   carries `.code`, `.message`, `.help`, and a `.span` (`offset` / `length` / `line` /
   `column`).
4. **Lint a template** with `lint` and inspect the structured findings
   (`LintResult.files`, `LintFileReport`, `LintDiagnostic`).
5. **Validate without rendering** with `check`, `check_file`, and `check_virtual`.
   Shows `CheckResult`, `MdsError` on a check failure (same `.code`/`.span` attributes
   as a compile failure), and the `source_map` rejection guard.
6. **In-memory virtual filesystem** with `compile_virtual` — compile from a `dict`
   of module sources, including cross-module `@import` with dependency tracking.
7. **Scan import paths** with `scan_imports` — returns `@extends` and `@import` paths
   found in a source string.
8. **File and virtual linting** with `lint_file` and `lint_virtual`.
9. **`lint_warnings`** — unknown rule names in the `rules` mapping produce a
   non-fatal warning (new in v0.4.0; previously silently ignored). Shows the
   `to_dict()` absent-when-empty behaviour and the mix of valid + unknown rules.
10. **Frozen + picklable result classes** — `pickle.loads(pickle.dumps(x))` roundtrips
    `CompileResult`, `CheckResult`, `LintResult`, and `Span`; mutation raises
    `AttributeError` (all result classes are `frozen`).

## Fixture file

`sample.mds` is a simple template used by sections 5 and 8:

- `check_file(SAMPLE_MDS)` returns `CheckResult(warnings=[])` — the file is valid.
- `lint_file(SAMPLE_MDS)` reports one `unused-variable` warning for the
  `unused_key` frontmatter key that is never referenced in the body.

## API quick reference

```python
import markdown_script

# Compile a source string. Keyword-only options.
r = markdown_script.compile(
    source,
    vars=None,            # dict of runtime variables
    base_path=None,       # directory for resolving @import in a string source
    source_map=False,     # attach a Source Map v3 document
    sources_content=False # embed original source text (requires source_map=True)
)

r.kind          # 'markdown' | 'messages'
r.output        # rendered Markdown (markdown kind)
r.messages      # list of Message (messages kind)
r.warnings      # list[str]
r.dependencies  # absolute paths of imported files
r.source_map    # dict | None  (None when source_map was not requested)
r.to_dict()     # plain dict; always includes "sourceMap": None when not requested
r.to_json()     # JSON string; omits "sourceMap" key when absent (canonical wire format)

# Compile a file, resolving @import relative to it.
markdown_script.compile_file(path, vars=None, source_map=False, sources_content=False)

# Compile from an in-memory dict of module sources.
# modules: {key: source_string}; entry must be a key in modules.
# @import paths are resolved against the entry key within the dict.
markdown_script.compile_virtual(modules, entry, vars=None, source_map=False, sources_content=False)

# Validate without rendering. Passing source_map or sources_content raises
# MdsError(code="mds::invalid_options") — source maps are a compile-only concept.
markdown_script.check(source, vars=None, base_path=None)
markdown_script.check_file(path, vars=None)
markdown_script.check_virtual(modules, entry, vars=None)

# Extract @extends and @import paths from a source string.
# Takes its argument positionally (not keyword-only).
imports = markdown_script.scan_imports(source)   # list[str]

# Lint. LintResult.files returns typed LintFileReport objects (B6/F10).
lr = markdown_script.lint(source, vars=None, base_path=None, rules=None)
markdown_script.lint_file(path, vars=None, rules=None)
markdown_script.lint_virtual(modules, entry, vars=None, rules=None)

lr.version, lr.truncated
lr.lint_warnings    # list[str] — non-fatal warnings, e.g. unknown rule names
for report in lr.files:          # list[LintFileReport]
    report.file                  # str — file key
    for diag in report.diagnostics:   # list[LintDiagnostic]
        diag.rule, diag.severity, diag.message
        diag.help                # str | None
        diag.fixable             # bool
        diag.fix_edits           # list[dict] | None
        diag.span                # Span | None
```

## Result classes

All result classes (`CompileResult`, `CheckResult`, `LintResult`, `LintDiagnostic`,
`LintFileReport`, `Message`, `Span`) are:

- **Frozen** — attributes are read-only; mutation raises `AttributeError`.
- **Picklable** — `pickle.loads(pickle.dumps(x))` round-trips value equality.
- **Unhashable** — `__hash__` is `None`; use `.to_dict()` or `.to_json()` for hashing.

## `lint_warnings` (v0.4.0)

When an unknown rule name is passed in the `rules` mapping:

- Prior to v0.4.0: the rule was silently ignored.
- v0.4.0+: the rule is still ignored, but its name is appended to `lr.lint_warnings`.
  The call succeeds; all recognised rules in the mapping are still applied normally.

The `lint_warnings` key is **absent** from `to_dict()` when there are no warnings
(idiomatic empty-absent convention, consistent with the TypeScript surface). The Python
attribute `lr.lint_warnings` always returns a list (empty when clean).

## Source map notes

- **`sources`** — string compiles report `["input.mds"]`. File compiles report
  paths relative to the project root (found via a `.git` / `.mdsroot` marker), or
  bare basenames when no marker is found above the file.
- **`file` key** — absent for binding results (it names the *output* artifact,
  which only the CLI knows). The CLI's sidecar `.map` sets it.
- **`sourcesContent`** — present only when `sources_content=True`. It ships the
  full template text; only embed it in trusted environments.
- **messages-mode** — templates built from `@message` blocks have no renderable
  positions, so `source_map` is `None` and a warning is added to `.warnings`.
- **cross-surface parity** — for the same input, the `mappings` string and
  `sources` list are byte-identical to the CLI, napi, and WASM surfaces.

See also [Project root](../../README.md#project-root) for how the `.git` / `.mdsroot`
marker governs root-relative `sources[]` paths in file compiles.
