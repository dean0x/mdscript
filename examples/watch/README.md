# Watching with `mds watch`

`mds watch` recompiles your templates automatically on every save. It tracks
transitive imports: editing a shared partial triggers a rebuild of every template
that depends on it, directly or indirectly. The watcher arms its file-system
watches and captures mtime baselines before the first read, so an edit made
during startup is never missed.

## Example layout

This example ships three source files:

- `prompt.mds` — entry template that imports `_persona.mds` and uses a variable
  from `vars.json`.
- `_persona.mds` — shared partial defining two macros (`intro` and `style`).
  The `_` prefix marks it as a partial: tracked in the reverse-dependency graph
  but never emitted to its own output file.
- `vars.json` — runtime variable overrides, reloaded on every rebuild.

Run every command below from the repository root.

## Step 1: Single-file mode, stream to stdout

**Terminal A** — start the watcher:

```bash
mds watch examples/watch/prompt.mds -o -
```

On stderr (illustrative — your path will differ):

```
Watching examples/watch/prompt.mds
```

On stdout, the compiled output appears immediately:

```
---
role: code reviewer
topic: Rust lifetime errors
name: Sage
---


You are a helpful code reviewer named Sage.

Your task: explain Rust lifetime errors to a junior developer.

Use plain language. Keep responses under 150 words.
```

**Terminal B** — open `examples/watch/prompt.mds` in your editor, change the
`topic` front-matter value to something else, and save. Terminal A immediately
prints the updated compiled output to stdout and writes a status line to stderr:

```
Recompiled <stdout> (1 deps) in 2ms
```

Press **Ctrl+C** in terminal A when done. Watch prints `Stopped watching.` to
stderr and exits 0.

## Step 2: Single-file mode, write to a file

```bash
mds watch examples/watch/prompt.mds -o /tmp/prompt-out.md
```

The compiled file is written to `/tmp/prompt-out.md` on startup and on every
subsequent rebuild. On each rebuild, stderr prints:

```
Recompiled /tmp/prompt-out.md (1 deps) in 2ms
```

## Step 3: Directory mode

Directory mode watches all `.mds` files under a root and mirrors output to a
separate directory, preserving the source subtree layout.

**Terminal A:**

```bash
mds watch examples/watch --out-dir /tmp/watch-demo
```

On stderr (illustrative):

```
Watching directory examples/watch
Compiled to /tmp/watch-demo/prompt.md
```

`_persona.mds` is a partial (`_`-prefixed) and is not emitted directly.

## Step 4: Editing a partial triggers its importers

This is the key cross-file behavior: `prompt.mds` imports `_persona.mds`, so the
watcher builds a reverse-dependency graph. Editing the partial causes every
template that imports it — directly or transitively — to recompile.

With directory mode still running from Step 3:

**Terminal B** — edit `examples/watch/_persona.mds`. Change the `style` macro
body to anything different and save. Terminal A immediately shows:

```
Recompiled /tmp/watch-demo/prompt.md (1 deps) in 1ms
```

The `(1 deps)` count confirms that `prompt.mds` was rebuilt because of its
dependency on the partial. You do not need to touch `prompt.mds` directly.

## Step 5: Runtime variable reload with `--vars`

The `--vars` flag accepts a JSON file that is reloaded from disk on every
rebuild. This lets you change variable values without restarting the watcher.

```bash
mds watch examples/watch/prompt.mds -o - --vars examples/watch/vars.json
```

`vars.json` supplies `name: "Ada"`, which overrides the `name: Sage` default in
the front matter:

```
You are a helpful code reviewer named Ada.
```

**Terminal B** — open `examples/watch/vars.json` and change the value:

```json
{
  "name": "Jordan"
}
```

Save. Terminal A recompiles and the output now reads:

```
You are a helpful code reviewer named Jordan.
```

You can also supply individual variables inline without a file:

```bash
mds watch examples/watch/prompt.mds -o - --set name=River --set-string topic="Go generics"
```

`--set` coerces the value to the natural JSON type; `--set-string` always treats
the value as a string (useful when a numeric-looking value must stay a string).

## Step 6: Tuning the watcher

### `--debounce`

The debounce window coalesces rapid-fire saves into a single rebuild. The
default is 100 ms, which is appropriate for most editors. Reduce it for
immediate feedback or raise it in noisy trees:

```bash
# Immediate rebuilds (no coalescing)
mds watch examples/watch/prompt.mds -o - --debounce 0

# Coalesce saves within a 500 ms window
mds watch examples/watch/prompt.mds -o - --debounce 500
```

### `--poll-interval`

The self-heal reconciler re-arms watches and checks file mtimes on each tick
(default 1000 ms). It exists as a backstop for changes that no OS event can
announce — for example, a cross-root dependency whose parent directory was
replaced. Reduce the interval for a more responsive backstop; disable it
entirely with `0` for pure OS-event mode:

```bash
# Check for missed changes every 500 ms
mds watch examples/watch --out-dir /tmp/watch-demo --poll-interval 500

# Disable self-heal (OS events only)
mds watch examples/watch --out-dir /tmp/watch-demo --poll-interval 0
```

### `--clear`

Clear the terminal before each rebuild, so the latest output is always at the
top. Only takes effect when stderr is a TTY (has no effect when output is
redirected):

```bash
mds watch examples/watch/prompt.mds -o - --clear
```

## Exit behavior

The watcher exits 0 on clean Ctrl+C. Compile errors during watching are printed
to stderr but never terminate the watcher — the process keeps watching and
retries on the next save. Only startup failures (missing entry file, unreadable
directory) produce a non-zero exit.
