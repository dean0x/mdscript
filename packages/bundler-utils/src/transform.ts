import type { MdsApi, MdsPluginOptions, TransformResult } from './types.js';
import { shouldTransform as checkTransform } from './frontmatter.js';
import { LazyInit } from './lazy-init.js';

// Characters that must be escaped inside a JS double-quoted string literal.
// U+2028 (line separator) and U+2029 (paragraph separator) are treated as
// line terminators in JS source and must be escaped even though JSON.stringify
// does not escape them. U+0000 (null byte) must be escaped to avoid truncation
// in C-style string handling downstream.
// Note: literal U+2028/U+2029 cannot appear in a regex literal (the parser treats
// them as line terminators), so the pattern is constructed via new RegExp().
const JS_ESCAPE_RE = new RegExp('[\\\\\"\\n\\r\\0\\u2028\\u2029]', 'g');
const JS_ESCAPE_MAP: Record<string, string> = {
  '\\': '\\\\',
  '"': '\\"',
  '\n': '\\n',
  '\r': '\\r',
  '\0': '\\0',
  ' ': '\\u2028',
  ' ': '\\u2029',
};

function escapeForJs(str: string): string {
  return str.replace(JS_ESCAPE_RE, (ch) => JS_ESCAPE_MAP[ch] ?? ch);
}

// Characters that are safe in JSON but unsafe when embedded inline in a
// <script> block: '<' can close the script tag (e.g. "</script>"); U+2028 and
// U+2029 are JS line terminators that JSON.stringify does not escape.
// Constructed via new RegExp() for the same reason as JS_ESCAPE_RE above.
const SAFE_JSON_RE = new RegExp('[<\\u2028\\u2029]', 'g');
const SAFE_JSON_MAP: Record<string, string> = {
  '<': '\\u003c',
  ' ': '\\u2028',
  ' ': '\\u2029',
};

/**
 * JSON-serialize a value for safe inline embedding in a JS script context.
 * JSON.stringify does not escape U+2028 (line separator), U+2029 (paragraph
 * separator), or '<' — all of which can break an inline <script> block or
 * be treated as JS line terminators. Escaping them to their Unicode escape
 * sequences is harmless for JSON consumers but safe for script contexts.
 */
function safeJsonForJs(value: unknown): string {
  return JSON.stringify(value).replace(SAFE_JSON_RE, (ch) => SAFE_JSON_MAP[ch] ?? ch);
}

/**
 * Create a transformer object that bundler plugins (Vite, Rollup, Webpack) use
 * to decide which module IDs to handle and to perform the actual compilation.
 *
 * The transformer is stateful: it lazily initialises the MDS compiler on first
 * use and reuses the same instance across all subsequent transform calls.
 *
 * @param mds - The MDS compiler API (satisfies {@link MdsApi}).  Pass the result
 *   of `import('@mdscript/mds')` or a compatible test double.
 * @param options - Optional plugin options.  `options.vars` are forwarded to
 *   every {@link MdsApi.compileFile} call as runtime template variables.
 * @returns An object with two methods:
 *   - `shouldTransform(id)` — returns `true` when `id` refers to an `.mds` file.
 *   - `transform(id)` — compiles the file and returns the generated JS module source.
 */
export function createMdsTransformer(mds: MdsApi, options?: MdsPluginOptions): {
  shouldTransform(id: string): boolean | Promise<boolean>;
  transform(id: string): Promise<TransformResult>;
} {
  const initLazy = new LazyInit<void>(async () => { await mds.init(); });

  return {
    shouldTransform: checkTransform,

    async transform(id: string): Promise<TransformResult> {
      await initLazy.get();
      // id is trusted — sourced from the bundler's module resolution pipeline.
      // Callers (vite-plugin, rollup-plugin, webpack-loader) are responsible for
      // stripping query/hash before calling transform().
      const result = await mds.compileFile(
        id,
        options?.vars !== undefined ? { vars: options.vars } : undefined,
      );
      const code =
        `export default "${escapeForJs(result.output)}";\n` +
        `export const metadata = ${safeJsonForJs({ warnings: result.warnings, dependencies: result.dependencies })};\n`;
      return {
        code,
        dependencies: result.dependencies,
        warnings: result.warnings,
      };
    },
  };
}
