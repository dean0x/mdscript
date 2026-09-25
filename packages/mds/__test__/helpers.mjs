/**
 * Shared test helpers for @mdscript/mds tests.
 */
import { fileURLToPath } from 'node:url';
import path from 'node:path';

export const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const FIXTURES = path.join(__dirname, 'fixtures');
export const SIMPLE_MDS = path.join(FIXTURES, 'simple.mds');
export const IMPORT_PROVIDER_MDS = path.join(FIXTURES, 'import_provider.mds');
export const IMPORT_CONSUMER_MDS = path.join(FIXTURES, 'import_consumer.mds');
export const ENTRY_MDS = path.join(FIXTURES, 'imports', 'entry.mds');
export const EMPTY_MDS = path.join(FIXTURES, 'edge', 'empty.mds');
export const FRONTMATTER_ONLY_MDS = path.join(FIXTURES, 'edge', 'frontmatter_only.mds');
export const MD_EXTENSION = path.join(FIXTURES, 'edge', 'md_extension.md');

function codepointRange(first, last) {
  return Array.from({ length: last - first + 1 }, (_, i) => first + i);
}

/**
 * The 80 codepoints #265 forbids in a path — C0 (TAB and LF included), DEL, C1,
 * and the bidi/format hazards U+061C, U+200E/U+200F, U+2028/U+2029,
 * U+202A–U+202E, U+2066–U+2069, U+FEFF. Stated here independently of both
 * implementations under test (Rust `mds::is_forbidden_path_char`, the JS
 * pre-scanner's `isForbiddenPathChar`), so a test driven by this list cannot
 * inherit a mistake from either.
 */
export const FORBIDDEN_PATH_CODEPOINTS = Object.freeze([
  ...codepointRange(0x00, 0x1f),
  0x7f,
  ...codepointRange(0x80, 0x9f),
  0x061c,
  0x200e,
  0x200f,
  0x2028,
  0x2029,
  ...codepointRange(0x202a, 0x202e),
  ...codepointRange(0x2066, 0x2069),
  0xfeff,
]);

const FORBIDDEN_SET = new Set(FORBIDDEN_PATH_CODEPOINTS);

function hex4(cp) {
  return cp.toString(16).toUpperCase().padStart(4, '0');
}

/** `U+XXXX` — how a refusal message names a codepoint. */
export function uPlus(cp) {
  return `U+${hex4(cp)}`;
}

/**
 * The six-character escape text (backslash, `u`, four uppercase hex digits) a
 * message shows in place of a forbidden codepoint. Built at runtime, never
 * written as a literal: the edit tooling decodes a literal one into the live
 * byte (PF-018).
 */
export function escapeText(cp) {
  return '\\u' + hex4(cp);
}

/**
 * Assert `s` carries none of the 80 forbidden codepoints — TAB and LF included,
 * unlike the older display-hazard helpers, which allow both.
 */
export function assertNoForbiddenChars(s, label) {
  for (const ch of s) {
    const cp = ch.codePointAt(0);
    if (FORBIDDEN_SET.has(cp)) {
      throw new Error(`${label}: raw forbidden ${uPlus(cp)} must not appear; got: ${JSON.stringify(s)}`);
    }
  }
}

/**
 * Run `fn` and return what it throws; fail when it returns normally, so a test
 * asserting on the error can never pass because nothing was thrown (PF-013).
 */
export function thrownBy(fn, label) {
  try {
    fn();
  } catch (err) {
    return err;
  }
  throw new Error(`${label}: expected a throw, but the call returned normally`);
}

/** Await `promise` and return its rejection reason; fail when it resolves (PF-013). */
export async function rejectionOf(promise, label) {
  try {
    await promise;
  } catch (err) {
    return err;
  }
  throw new Error(`${label}: expected a rejection, but the promise resolved`);
}

/** The binding-visible fields of an MDS error, for cross-backend `deepEqual`. */
export function errorShape(err) {
  return {
    code: err.code,
    message: err.message,
    help: err.help ?? null,
    span: err.span ?? null,
  };
}
