# Python bindings

`mdscript` is the native Python binding for the MDS compiler (built with PyO3).
It exposes the same compiler as the CLI and the JavaScript packages, including
**Source Map v3** generation.

## Setup

The bindings are installed into the repo's virtualenv with
[maturin](https://www.maturin.rs/). From the repository root:

```bash
python -m venv .venv
source .venv/bin/activate
pip install "maturin==1.13.3" pytest
maturin develop -m crates/mds-python/Cargo.toml
```

Once the venv is active, `import mdscript` works.

## Run the demo

```bash
source .venv/bin/activate
python examples/python/demo.py
```

`demo.py` walks through four things:

1. **Compile a string** to Markdown with runtime `vars`.
2. **Generate a source map** with `source_map=True` and read it back — plus
   `compile_file(..., sources_content=True)` to embed the original template text.
3. **Handle errors** — a failed compile raises `mdscript.MdsError`, which carries
   `.code`, `.help`, and a `.span` (`offset` / `length` / `line` / `column`).
4. **Lint** a template and inspect the structured findings.

## API quick reference

```python
import mdscript

# Compile a source string. Keyword-only options.
r = mdscript.compile(
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
mdscript.compile_file(path, vars=None, source_map=False, sources_content=False)

# Validate without rendering. Note: check() takes NO source_map argument —
# passing one raises TypeError, since source maps are a compile-only concept.
mdscript.check(source, vars=None, base_path=None)

# Lint. LintResult.files returns typed LintFileReport objects (B6/F10).
lr = mdscript.lint(source, vars=None, base_path=None, rules=None)
lr.version, lr.truncated
for report in lr.files:          # list[LintFileReport]
    report.file                  # str — file key
    for diag in report.diagnostics:   # list[LintDiagnostic]
        diag.rule, diag.severity, diag.message
        diag.help                # str | None
        diag.fixable             # bool
        diag.span                # Span | None
```

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
