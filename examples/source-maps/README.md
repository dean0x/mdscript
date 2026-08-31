# Source maps

`annotated-prompt.mds` imports helpers from the `_style.mds` partial, so its
source map traces compiled lines back to **two** source files.

## Build with a source map

From the repository root:

```bash
mds build examples/source-maps/annotated-prompt.mds --source-map
```

This writes two files next to the template:

- `annotated-prompt.md` — the compiled output (byte-identical to a build
  without `--source-map`)
- `annotated-prompt.md.map` — the sidecar map, named `<output>.map` (the
  `.map` extension is appended to the full output filename)

```
Compiled to examples/source-maps/annotated-prompt.md
Source map written to examples/source-maps/annotated-prompt.md.map
```

> **Gitignore note:** `annotated-prompt.md.map` is written next to the source
> file and is covered by this repo's `.gitignore` (pattern
> `examples/**/*.md.map`). Running the command above leaves no untracked file
> in `git status`; writing maps in-tree is safe.

## How to read the map

The sidecar is standard [Source Map v3](https://tc39.es/ecma426/) JSON:

```json
{
  "version": 3,
  "file": "annotated-prompt.md",
  "sources": ["annotated-prompt.mds", "_style.mds"],
  "names": [],
  "mappings": ";;;;;;;;AAQA;..."
}
```

`sources` lists every file that contributed output — the entry template plus
each `@import`/`@extends` module. The path encoding follows a two-level
anchoring rule:

- **Map inside the project tree:** paths are **relative to the map file**.
  When the output (and therefore the map) is written within the project root,
  `sources` are resolved relative to the map directory. For example, building
  within `examples/source-maps/` gives bare filenames
  (`annotated-prompt.mds`, `_style.mds`) because sources and map share the
  same directory.
- **Map outside the project tree:** when `-o` points outside the project
  root — for example `mds build … -o /tmp/annotated-prompt.md` — the map
  directory is not contained within the root, so the algorithm falls back to
  **project-root-relative** paths (`examples/source-maps/annotated-prompt.mds`,
  `examples/source-maps/_style.mds`). These paths never expose an absolute
  filesystem location, but they are not resolvable from the map's actual
  destination; they are resolvable from the project root.

`mappings` is Base64-VLQ data: one `;`-separated group per generated line,
each segment mapping a generated column to `(source index, line, column)`.
Any standard source-map consumer (`source-map` on npm, `sourcemap` on PyPI)
can decode it. Content produced by an imported module (for example the
`## Focus areas` heading from `_style.mds`) maps back to the *module* file,
not the entry template.

## Reading a map from JavaScript

[`consume-map.mjs`](consume-map.mjs) compiles this template through the
`@mdscript/mds` API with `sourceMap: true`, runs a **security assertion** to
verify that `sources[]` contains no absolute filesystem paths, decodes the
Base64-VLQ `mappings`, and traces individual output lines back to their source
file and line:

```bash
node examples/source-maps/consume-map.mjs
```

```
Backend: native

Security check passed: sources[] has 2 entries, none absolute.

Source Map v3 document:
  version:  3
  sources:  ["examples/source-maps/annotated-prompt.mds","examples/source-maps/_style.mds"]
  names:    []
  file key present? false  (bindings omit it; the CLI sets it)

Tracing three output lines back to source:

generated L10: "# System prompt: release reviewer"
  col 17 → examples/source-maps/annotated-prompt.mds:11:18
  col 33 → examples/source-maps/annotated-prompt.mds:11:27
generated L14: "## Focus areas"
  col 0 → examples/source-maps/_style.mds:2:1
  col 3 → examples/source-maps/_style.mds:2:4
  col 14 → examples/source-maps/annotated-prompt.mds:15:33
generated L16: "- changelog accuracy"
  col 0 → examples/source-maps/_style.mds:6:1
  col 2 → examples/source-maps/_style.mds:6:3
  col 20 → examples/source-maps/annotated-prompt.mds:18:21

Each imported-module line maps back to _style.mds, while the
interpolated values map back to annotated-prompt.mds — exactly the
provenance a debugger or prompt-inspection tool needs.
```

The same script runs on either backend (`MDS_BACKEND=wasm node …`) and
produces identical mappings and an identical security-check result.

Binding results differ from the CLI in-tree sidecar in two documented ways:
the `sourceMap.file` key is **absent** (bindings do not know the output path),
and `sources` are always **project-root-relative** because bindings supply no
map-directory anchor. Note that the root-relative fallback is not exclusive to
bindings — the CLI produces root-relative paths too whenever the output
destination is outside the project tree (see "Map outside the project tree"
above). What distinguishes the CLI in-tree case is that it knows the output
directory and can emit map-relative paths when that directory is inside the
root.

## Inline variant

Embed the map as a data-URI comment in the output file (no sidecar written):

```bash
mds build examples/source-maps/annotated-prompt.mds --source-map --inline -o /tmp/annotated-prompt.md
```

```
Compiled to /tmp/annotated-prompt.md
```

No `Source map written to …` line appears — the map is embedded at the end of
the output file instead:

```
<!--# sourceMappingURL=data:application/json;base64,eyJ2ZXJzaW9uIjozLC... -->
```

The `sources` in the inline map follow the same two-level path rule as the
sidecar. Here the output is in `/tmp/` (outside the project root), so the
inline map carries root-relative sources
(`examples/source-maps/annotated-prompt.mds`, `examples/source-maps/_style.mds`).

## Embedding source text (`--embed-sources`)

Fill `sourcesContent[]` with the full text of each source file, making the
map self-contained (no access to the `.mds` files needed to inspect sources):

```bash
mds build examples/source-maps/annotated-prompt.mds --source-map --embed-sources -o /tmp/annotated-prompt.md
```

```
Compiled to /tmp/annotated-prompt.md
Source map written to /tmp/annotated-prompt.md.map
```

The sidecar now includes a `sourcesContent` array parallel to `sources`:

```json
{
  "version": 3,
  "sources": ["examples/source-maps/annotated-prompt.mds", "examples/source-maps/_style.mds"],
  "sourcesContent": [
    "---\nagent: release reviewer\n...",
    "@define section(title):\n## {{title}}\n@end\n..."
  ],
  "names": [],
  "mappings": "..."
}
```

> **Privacy caveat:** `--embed-sources` ships your complete template text —
> including any comments and internal prompt engineering — inside the map.
> Do not distribute such maps with output you consider the templates
> confidential to. The default (no `--embed-sources`) omits `sourcesContent`.

## Inline map with embedded sources

Combine `--inline` and `--embed-sources` for a fully self-contained single
file — no sidecar, no separate source files needed to decode mappings:

```bash
mds build examples/source-maps/annotated-prompt.mds --source-map --inline --embed-sources -o /tmp/annotated-prompt.md
```

```
warning: --embed-sources with --inline ships full source text in the output (AC-SEC-02)
Compiled to /tmp/annotated-prompt.md
```

The output file ends with a single HTML comment carrying a Base64-encoded
Source Map v3 document that contains both `mappings` and the full
`sourcesContent` of every `.mds` source. No sidecar is written; the map and
sources travel with the compiled output.

## Config: `mds.json`

Set `build.source_map = true` in `mds.json` to enable source maps project-wide
without passing `--source-map` on every invocation. The reference config file
for this example is [`config-demo/mds.json`](config-demo/mds.json):

```json
{
  "build": {
    "source_map": true
  }
}
```

Place `mds.json` next to your templates (or in any parent directory up to the
project root). Every subsequent `mds build` picks it up and emits a sidecar
automatically:

```
Compiled to annotated-prompt.md
Source map written to annotated-prompt.md.map
```

**Graceful degradation (v0.4.0):** If `source_map = true` is set in config but
the build writes to stdout (`-o -`), the sidecar cannot be written. In v0.4.0
the tool emits a single warning and exits 0 rather than failing:

```
warning: source_map in <config-dir> has no effect when writing to stdout
(sidecar requires -o <file> or --out-dir); use --inline to embed the map,
or --no-source-map to silence this warning
```

**Override:** pass `--no-source-map` to suppress source-map generation for a
single build even when the config enables it. If a stale sidecar from a prior
build exists, it is removed:

```
Compiled to annotated-prompt.md
Removed stale map annotated-prompt.md.map
```

## Messages-mode templates

Templates that use `@message` blocks compile to a JSON messages array rather
than Markdown. Source maps operate on a flat text stream and are incompatible
with the `@message` boundary model. Passing `--source-map` with a messages-mode
template emits a single warning on stderr and exits 0 — no map file is written:

```bash
mds build examples/source-maps/messages-demo.mds --source-map -o /tmp/messages-demo.json
```

```
source maps are not supported for messages-mode templates (@message blocks); no source map will be generated
Compiled to /tmp/messages-demo.json
```

The warning fires exactly once (v0.4.0 deduplication), and the exit code is 0.

See also [Project root](../../README.md#project-root) for how the `.git` / `.mdsroot`
marker determines the root-relative path anchor used by binding surfaces for `sources[]`.
