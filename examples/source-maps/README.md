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

- `sources` lists every file that contributed output, as paths **relative to
  the map file** — the entry template plus each `@import`/`@extends` module.
- `mappings` is Base64-VLQ data: one `;`-separated group per generated line,
  each segment mapping a generated column to `(source index, line, column)`.
  Any standard source-map consumer (`source-map` on npm, `sourcemap` on PyPI)
  can decode it. Content produced by an imported module (for example the
  `## Focus areas` heading from `_style.mds`) maps back to the *module* file,
  not the entry template.

## Reading a map from JavaScript

[`consume-map.mjs`](consume-map.mjs) compiles this template through the
`@mdscript/mds` API with `sourceMap: true`, decodes the Base64-VLQ `mappings`,
and traces individual output lines back to their source file and line:

```bash
node examples/source-maps/consume-map.mjs
```

It prints, for example, that generated line 14 (`## Focus areas`) maps back to
`_style.mds` (the `section()` macro body) while its interpolated title maps to
`annotated-prompt.mds`. The same script runs on either backend
(`MDS_BACKEND=wasm node …`) and produces identical mappings.

Binding results differ from the CLI sidecar in two documented ways: the
`sourceMap.file` key is **absent** (bindings do not know the output path), and
`sources` are **project-root-relative** (resolved via a `.git` / `.mdsroot`
marker) rather than map-relative.

## Inline variant

```bash
mds build examples/source-maps/annotated-prompt.mds --source-map --inline
```

No sidecar is written; instead the map is appended to the output as a final
HTML comment:

```
<!--# sourceMappingURL=data:application/json;base64,eyJ2ZXJzaW9uIjozLC... -->
```

## Embedding source text

```bash
mds build examples/source-maps/annotated-prompt.mds --source-map --embed-sources
```

This fills `sourcesContent` with the full text of each source file, making the
map self-contained (no access to the `.mds` files needed to inspect sources).

> **Privacy caveat:** `--embed-sources` ships your complete template text —
> including any comments and internal prompt engineering — inside the map.
> Do not distribute such maps with output you consider the templates
> confidential to. The default (no `--embed-sources`) omits `sourcesContent`.

See also [Project root](../../README.md#project-root) for how the `.git` / `.mdsroot`
marker determines the root-relative path anchor used by binding surfaces for `sources[]`.
