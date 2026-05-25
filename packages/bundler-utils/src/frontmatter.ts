import { open } from 'node:fs/promises';

/** Returns `true` if `id` has a `.mds` file extension. */
export function isMdsExtension(id: string): boolean {
  return id.endsWith('.mds');
}

/** Strips query string and hash fragment from a module id, returning the bare file path. */
export function cleanId(id: string): string {
  const qIdx = id.indexOf('?');
  const hIdx = id.indexOf('#');
  if (qIdx === -1 && hIdx === -1) return id;
  const cutAt = Math.min(
    qIdx === -1 ? id.length : qIdx,
    hIdx === -1 ? id.length : hIdx,
  );
  return id.slice(0, cutAt);
}

/**
 * Checks whether a file should be transformed by the MDS bundler plugin.
 *
 * - `.mds` files: always transform (synchronous true)
 * - `.md` files with `type: mds` inside their frontmatter block: transform (async)
 * - Everything else: skip (synchronous false or async false)
 *
 * Frontmatter detection reads only the first 500 bytes and looks for:
 * 1. File starts with `---`
 * 2. There is a closing `---` before byte 500
 * 3. Between the opening and closing `---`, there is a `type: mds` key
 */
export function shouldTransform(id: string): boolean | Promise<boolean> {
  // id is expected to be pre-cleaned by the caller (query/hash stripped).
  // Callers (vite-plugin, rollup-plugin) call cleanId() before invoking this.
  if (isMdsExtension(id)) return true;
  if (!id.endsWith('.md')) return false;

  // Async: read only the first 512 bytes and check for type: mds in frontmatter.
  // Using open + read instead of readFile avoids loading the entire file into memory
  // for large .md files.
  const PEEK_BYTES = 512;
  return open(id, 'r')
    .then(async (fh) => {
      try {
        const buf = Buffer.alloc(PEEK_BYTES);
        const { bytesRead } = await fh.read(buf, 0, PEEK_BYTES, 0);
        const head = buf.toString('utf-8', 0, bytesRead);
        if (!head.startsWith('---')) return false;

        // Find the closing --- (must be after the opening line, i.e. after index 3)
        const closeIdx = head.indexOf('\n---', 3);
        if (closeIdx === -1) return false;

        // Extract frontmatter block (between opening --- and closing ---)
        const frontmatter = head.slice(3, closeIdx);

        // Check for `type: mds` as a YAML key (at start of line or after whitespace)
        return /(?:^|\n)\s*type:\s*mds\b/.test(frontmatter);
      } finally {
        await fh.close();
      }
    })
    .catch(() => false);
}
