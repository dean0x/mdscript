# MDS × Vite example

Demonstrates [`@mdscript/vite-plugin`](../../packages/vite-plugin) compiling
`.mds` (and `.md` with `type: mds` frontmatter) prompt templates into ES modules
— both at build time and with **live HMR** in the dev server.

## Build

```bash
npm install
npm run build      # vite build (library mode) → dist/main.js
```

`dist/main.js` contains the compiled prompts as exported strings.

## Live HMR demo

```bash
npm run dev        # vite dev server, http://localhost:5173
```

Open the page, then edit either:

- `src/prompts/reviewer.mds` — the prompt imported by the page, or
- `src/prompts/rules.mds` — a transitive `@import` dependency of `reviewer.mds`

Save, and the page hot-reloads with the recompiled prompt. The `rules.mds` case
specifically exercises the plugin's **transitive-dependency tracking**: a change
to a file you never imported *directly* still triggers a reload, because the
plugin records every `@import` dependency it compiles and watches it.
