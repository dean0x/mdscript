import type { MdsApi, MdsPluginOptions, TransformResult } from './types.js';
import { shouldTransform as checkTransform, cleanId } from './frontmatter.js';

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

export function createMdsTransformer(mds: MdsApi, options?: MdsPluginOptions): {
  shouldTransform(id: string): boolean | Promise<boolean>;
  transform(id: string): Promise<TransformResult>;
} {
  let initialized = false;
  let initPromise: Promise<void> | null = null;

  async function ensureInit(): Promise<void> {
    if (initialized) return;
    if (initPromise === null) {
      initPromise = mds.init().then(
        () => { initialized = true; },
        (err: unknown) => { initPromise = null; throw err; },
      );
    }
    return initPromise;
  }

  return {
    shouldTransform: checkTransform,

    async transform(id: string): Promise<TransformResult> {
      await ensureInit();
      const clean = cleanId(id);
      // id is trusted — sourced from the bundler's module resolution pipeline
      const result = await mds.compileFile(
        clean,
        options?.vars !== undefined ? { vars: options.vars } : undefined,
      );
      const code =
        `export default "${escapeForJs(result.output)}";\n` +
        `export const metadata = ${JSON.stringify({ warnings: result.warnings, dependencies: result.dependencies })};\n`;
      return {
        code,
        dependencies: result.dependencies,
        warnings: result.warnings,
      };
    },
  };
}
