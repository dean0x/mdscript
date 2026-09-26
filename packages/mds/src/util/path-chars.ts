/**
 * Forbidden path characters (#265) — the JS pre-scanner's copy of the Rust rule.
 *
 * `@mdscript/mds`'s WASM backend opens a file entry and its imports in JS before
 * the Rust engine sees them, so it applies the rule the Rust resolver and
 * `NativeFs` apply: the same 80-codepoint class, classified in the same order,
 * refused with the same message text. Keep this module in lockstep with
 * `mds::is_forbidden_path_char` and `mds::escape_path_for_message`
 * (crates/mds-core/src/lint/diagnostic.rs), `forbidden_char_message`
 * (crates/mds-core/src/fs.rs) and `import_path_violation`
 * (crates/mds-core/src/resolver.rs). `__test__/forbidden-path-chars.spec.mjs`
 * compares the two against the real engine on both backends; a change on one
 * side alone fails it.
 *
 * Pure functions only — no I/O — so it is browser-safe.
 */

/**
 * Returns `true` when `cp` must be refused in a path accepted from untrusted
 * input — mirrors Rust `mds::is_forbidden_path_char`.
 *
 * The class is 80 codepoints: every C0 control (TAB and LF included), DEL,
 * every C1 control, and the display hazards — U+061C ARABIC LETTER MARK, the
 * other bidi controls (U+200E/U+200F, U+202A–U+202E, U+2066–U+2069), U+2028
 * LINE SEPARATOR / U+2029 PARAGRAPH SEPARATOR, and U+FEFF BOM.
 */
export function isForbiddenPathChar(cp: number): boolean {
  return (
    cp <= 0x1f ||
    cp === 0x7f ||
    (cp >= 0x80 && cp <= 0x9f) ||
    cp === 0x061c ||
    cp === 0x200e ||
    cp === 0x200f ||
    cp === 0x2028 ||
    cp === 0x2029 ||
    (cp >= 0x202a && cp <= 0x202e) ||
    (cp >= 0x2066 && cp <= 0x2069) ||
    cp === 0xfeff
  );
}

/** Four uppercase hex digits — every member of the class is in the BMP. */
function hex4(cp: number): string {
  return cp.toString(16).toUpperCase().padStart(4, '0');
}

/** The first forbidden codepoint of `path`, if any. */
export function firstForbiddenChar(path: string): number | undefined {
  for (const ch of path) {
    const cp = ch.codePointAt(0);
    if (cp !== undefined && isForbiddenPathChar(cp)) return cp;
  }
  return undefined;
}

/**
 * Escape every forbidden codepoint in `path` to the six-character text a
 * message shows for it (backslash, `u`, four uppercase hex digits) — mirrors
 * Rust `mds::escape_path_for_message`. The result carries none of the 80.
 */
export function escapePathForMessage(path: string): string {
  let out = '';
  for (const ch of path) {
    const cp = ch.codePointAt(0);
    out += cp !== undefined && isForbiddenPathChar(cp) ? '\\u' + hex4(cp) : ch;
  }
  return out;
}

/**
 * `<what> contains forbidden character U+XXXX: "<shown, escaped>"` — the Rust
 * `forbidden_char_message`. `shown` is the path as the caller wrote it, never a
 * resolved absolute path.
 */
export function forbiddenCharMessage(what: string, cp: number, shown: string): string {
  return `${what} contains forbidden character U+${hex4(cp)}: "${escapePathForMessage(shown)}"`;
}

/** The first rule an import string breaks — the Rust `ImportPathViolation`. */
export type ImportPathViolation =
  | { readonly kind: 'not-relative' }
  | { readonly kind: 'null-byte' }
  | { readonly kind: 'forbidden-char'; readonly codePoint: number };

/**
 * Classify an `@import` / `@export … from` / `@extends` path the way the Rust
 * resolver does (`import_path_violation`): relative form first, then NUL (which
 * keeps its own message although U+0000 is in the class), then the rest of the
 * class. Returns `undefined` for an acceptable path.
 */
export function importPathViolation(path: string): ImportPathViolation | undefined {
  if (!path.startsWith('./') && !path.startsWith('../')) {
    return { kind: 'not-relative' };
  }
  if (path.includes('\0')) {
    return { kind: 'null-byte' };
  }
  const codePoint = firstForbiddenChar(path);
  return codePoint === undefined ? undefined : { kind: 'forbidden-char', codePoint };
}
