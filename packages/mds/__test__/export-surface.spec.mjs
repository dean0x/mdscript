import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const SHARED_TYPES = [
  'BackendType',
  'CheckOptions',
  'CheckResult',
  'CompileOptions',
  'CompileResult',
  'InitOptions',
  'LintDiagnostic',
  'LintFileOptions',
  'LintFileReport',
  'LintOptions',
  'LintResult',
  'LintRuleName',
  'LintSpan',
  'MarkdownResult',
  'MdsError',
  'MdsErrorSpan',
  'Message',
  'MessagesResult',
  'RuleSeverity',
  'SourceMapV3',
];
const NODE_ONLY_TYPES = ['CheckFileOptions', 'CompileFileOptions'];
const INTERNAL_TYPES = ['MdsBaseBackend', 'MdsNodeBackend'];
const SHARED_VALUES = ['LINT_RULE_NAMES', 'isMdsError'];
const SHARED_FUNCTIONS = ['_resetForTesting', 'check', 'compile', 'getBackend', 'init', 'lint', 'lintVirtual'];
const NODE_FUNCTIONS = [...SHARED_FUNCTIONS, 'checkFile', 'compileFile', 'lintFile'];
const BROWSER_FUNCTIONS = [...SHARED_FUNCTIONS, '_initWithModuleForTesting'];
const SHARED_RE_EXPORT = '* from public-types';

function namesWithKind(kind, names) {
  return names.map((name) => `${kind} ${name}`);
}

function readSourceExports(file) {
  const source = readFileSync(new URL(`../src/${file}.ts`, import.meta.url), 'utf8');
  // Skip comments and quoted text. Reject export forms outside the forms below.
  const tokens = /\/\/[^\r\n]*|\/\*[\s\S]*?\*\/|'(?:\\[\s\S]|[^'\\])*'|"(?:\\[\s\S]|[^"\\])*"|`(?:\\[\s\S]|[^`\\])*`|\b(export)\b/g;
  return [...source.matchAll(tokens)].flatMap((token) => {
    if (!token[1]) return [];
    const statement = source.slice(token.index);
    const declaration = /^export\s+(interface|type|function)\s+([A-Za-z_$][\w$]*)(?=\s*(?:[=({<]|extends\b))/.exec(statement);
    if (declaration) {
      const [, form, name] = declaration;
      return namesWithKind(form === 'function' ? 'value' : 'type', [name]);
    }
    const array = /^export\s+const\s+([A-Za-z_$][\w$]*)\s*:\s*readonly\s+\w+\[\]\s*=\s*\[[^\]]*\](?:\s+as\s+const)?\s*;/.exec(statement);
    if (array) return namesWithKind('value', [array[1]]);

    const list = /^export\s+(type\s+)?\{([^}]*)\}\s+from\s+(['"])\.\/types\.js\3\s*;/.exec(statement);
    if (list) {
      const names = list[2].trim().replace(/,$/, '').split(',').map((name) => name.trim());
      for (const name of names) {
        assert.match(name, /^[A-Za-z_$][\w$]*$/, `${file}.ts: unknown export specifier ${JSON.stringify(name)}`);
      }
      return namesWithKind(list[1] ? 'type' : 'value', names);
    }
    if (/^export\s*\*\s+from\s+(['"])\.\/public-types\.js\1\s*;/.test(statement)) return [SHARED_RE_EXPORT];

    assert.fail(`${file}.ts: unknown export form: ${statement.split('\n', 1)[0]}`);
  });
}

const SHARED_EXPORTS = [...namesWithKind('type', SHARED_TYPES), ...namesWithKind('value', SHARED_VALUES)];

test('U-ES-1: types.ts exports only the shared, Node.js-only, and internal names', () => {
  assert.deepEqual(readSourceExports('types').sort(), [
    ...SHARED_EXPORTS,
    ...namesWithKind('type', [...NODE_ONLY_TYPES, ...INTERNAL_TYPES]),
  ].sort());
});

test('U-ES-2: public-types.ts exports exactly the shared names', () => {
  assert.deepEqual(readSourceExports('public-types').sort(), SHARED_EXPORTS.toSorted());
});

test('U-ES-3: the Node.js source exports the shared list and its own names', () => {
  assert.deepEqual(readSourceExports('node').sort(), [
    SHARED_RE_EXPORT,
    ...namesWithKind('type', NODE_ONLY_TYPES),
    ...namesWithKind('value', NODE_FUNCTIONS),
  ].sort());
});

test('U-ES-4: the browser source exports the shared list and its own functions', () => {
  assert.deepEqual(readSourceExports('browser').sort(), [
    SHARED_RE_EXPORT,
    ...namesWithKind('value', BROWSER_FUNCTIONS),
  ].sort());
});

test('U-ES-5: the Node.js build exports exactly its expected values', async () => {
  const node = await import('../dist/node.js');
  assert.deepEqual(Object.keys(node).sort(), [...SHARED_VALUES, ...NODE_FUNCTIONS].sort());
});

test('U-ES-6: the browser build exports exactly its expected values', async () => {
  const browser = await import('../dist/browser.js');
  assert.deepEqual(Object.keys(browser).sort(), [...SHARED_VALUES, ...BROWSER_FUNCTIONS].sort());
});
