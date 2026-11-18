/**
 * Backend contract parity tests for @mdscript/mds.
 * Tests: U-BC1 through U-BC20
 *
 * Shape tests use stub backends. Conversion parity tests require current native
 * and WASM builds and fail if either backend is absent.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { compileFileOutcomes } from './helpers.mjs';
import {
  assertResultShape,
  validateBackendMethods,
  BASE_METHODS,
  NODE_METHODS,
  WASM_EXPORTS,
} from '../dist/backend/contract.js';
import { createWasmBackend, initWasmNode } from '../dist/backend/wasm.js';
import { createNativeBackend } from '../dist/backend/native.js';

// Both live backends must reject the same unsupported value at the same field.
test('native and WASM conversion errors share their code and field prefix', async () => {
  const require = createRequire(import.meta.url);
  const native = createNativeBackend(require('../../../crates/mds-napi/mds-napi.node'));
  const wasm = createWasmBackend(await initWasmNode());
  const prefix = 'invalid options.vars: ';
  const summarize = (backend) => {
    let error;
    try {
      backend.compile('ok', { vars: { bad: Symbol('unsupported') } });
    } catch (caught) {
      error = caught;
    }
    assert.ok(error instanceof Error, 'the unsupported value must reach the converter');
    return [error.code, error.message.slice(0, prefix.length)];
  };
  assert.deepEqual(
    [summarize(native), summarize(wasm)],
    Array.from({ length: 2 }, () => ['mds::invalid_options', prefix]),
  );
  const assertClean = (backend) => assert.equal(
    backend.compile('{{value}}', { vars: { value: 'ok' } }).output,
    'ok\n',
  );
  assertClean(native);
  assertClean(wasm);
  assert.throws(
    () => assertClean({ compile: () => ({ output: 'wrong' }) }),
    assert.AssertionError,
  );
});

test('native and WASM import refusals share code, message, span and importer source', async (t) => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'mds-not-mds-parity-'));
  try {
    await mkdir(path.join(root, 'sub'));
    await writeFile(path.join(root, '.mdsroot'), '');
    const entry = path.join(root, 'a.mds');
    const importer = path.join(root, 'sub', 'b.mds');
    const entrySource = '@import "./sub/b.mds" as b\nEntry\n';
    const targetSource = '@import "./never.mds"\n';
    const directives = [
      ['alias import', '@import "./c.txt" as c'],
      ['merge import', '@import "./c.txt"'],
      ['selective import', '@import { f } from "./c.txt"'],
      ['named re-export', '@export f from "./c.txt"'],
      ['wildcard re-export', '@export * from "./c.txt"'],
      ['extends', '@extends "./c.txt"'],
      ['include namespace import', '@import "./c.txt" as c\n@include c'],
    ];
    await writeFile(entry, entrySource);
    await writeFile(path.join(root, 'sub', 'c.txt'), targetSource);
    await writeFile(path.join(root, 'sub', 'c.mds'), '@define f():\nOK\n@end\nBody\n');
    const cases = directives.flatMap(([label, directive]) => [
      [label, directive, 'c.txt', 'mds::not_mds', 'not an MDS file: ./c.txt'],
      [label, directive.replace('c.txt', 'missing.mds'), 'missing.mds', 'mds::file_not_found', 'file not found: ./missing.mds'],
    ]);
    for (const [label, directive, target, code, message] of cases) {
      await t.test(`${label}: ${code}`, async () => {
        const prefix = label === 'extends' ? '\n'.repeat(112) : '# Importer é\n'.repeat(8);
        const source = `${prefix}${directive}\n`;
        await writeFile(importer, source.replace(target, 'c.mds'));
        const cleanNative = await compileFileOutcomes('native', [entry]);
        const cleanWasm = await compileFileOutcomes('wasm', [entry]);
        assert.ok(Object.hasOwn(cleanNative[0], 'output'), 'the native control must compile');
        assert.deepEqual(cleanWasm, cleanNative, 'the valid target must compile on both backends');
        await writeFile(importer, source);
        const [native] = await compileFileOutcomes('native', [entry]);
        const [wasm] = await compileFileOutcomes('wasm', [entry]);
        const sources = { 'a.mds': entrySource, 'sub/b.mds': source, 'sub/c.txt': targetSource };
        const line = directive.split('\n')[0];
        const summarize = (err) => {
          // The JS error has no source-name field. Identify the source by its unique
          // byte slice instead of assuming that an in-bounds span belongs to b.mds.
          const files = Object.entries(sources).filter(([, text]) => err.span != null &&
            Buffer.from(text).subarray(err.span.offset, err.span.offset + err.span.length).toString() === line,
          ).map(([file]) => file);
          return [err.code, err.message, err.span?.offset ?? null, err.span?.length ?? null, files];
        };
        const expected = [code, message, Buffer.byteLength(prefix), Buffer.byteLength(line), ['sub/b.mds']];
        // Compare WASM directly with native, then pin the reference to the importer.
        assert.deepEqual([summarize(wasm), summarize(native)], [summarize(native), expected], label);
      });
    }
    await t.test('intermediate extends: mds::not_mds', async () => {
      const base = path.join(root, 'sub', 'c.mds');
      const importerSource = '@extends "./c.mds"\n';
      const directive = '@extends "./d.txt"';
      const prefix = '\n'.repeat(23);
      const source = `${prefix}${directive}\n`;
      await writeFile(importer, importerSource);
      await writeFile(path.join(root, 'sub', 'd.txt'), targetSource);
      await writeFile(path.join(root, 'sub', 'd.mds'), 'Body\n');
      await writeFile(base, source.replace('d.txt', 'd.mds'));
      const cleanNative = await compileFileOutcomes('native', [entry]);
      const cleanWasm = await compileFileOutcomes('wasm', [entry]);
      assert.ok(Object.hasOwn(cleanNative[0], 'output'), 'the intermediate-base control must compile');
      assert.deepEqual(cleanWasm, cleanNative, 'the intermediate-base control must match on both backends');
      await writeFile(base, source);
      const [native] = await compileFileOutcomes('native', [entry]);
      const [wasm] = await compileFileOutcomes('wasm', [entry]);
      const sources = {
        'a.mds': entrySource, 'sub/b.mds': importerSource,
        'sub/c.mds': source, 'sub/d.txt': targetSource,
      };
      const summarize = (err) => {
        const files = Object.entries(sources).filter(([, text]) => err.span != null &&
          Buffer.from(text).subarray(err.span.offset, err.span.offset + err.span.length).toString() === directive,
        ).map(([file]) => file);
        return [err.code, err.message, err.span?.offset ?? null, err.span?.length ?? null, files];
      };
      const expected = ['mds::not_mds', 'not an MDS file: ./d.txt', Buffer.byteLength(prefix), Buffer.byteLength(directive), ['sub/c.mds']];
      assert.deepEqual(wasm, native, 'the complete intermediate-base errors must match');
      assert.deepEqual([summarize(wasm), summarize(native)], [summarize(native), expected]);
    });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('native and WASM nested frontmatter failures keep their code and no outer span', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'mds-frontmatter-parity-'));
  try {
    await mkdir(path.join(root, 'sub'));
    await writeFile(path.join(root, '.mdsroot'), '');
    const entry = path.join(root, 'a.mds');
    const importer = path.join(root, 'sub', 'b.mds');
    await writeFile(entry, '\n\n@import "./sub/b.mds" as b\nEntry\n');
    await writeFile(path.join(root, 'sub', 'c.txt'), 'Text\n');
    await writeFile(path.join(root, 'sub', 'c.mds'), 'Body\n');
    for (const [target, code, message] of [
      ['c.mds', null, null],
      ['missing.mds', 'mds::import', 'import error: file not found: "./missing.mds" (in frontmatter imports[0])'],
      ['c.txt', 'mds::not_mds', 'not an MDS file: ./c.txt'],
    ]) {
      await writeFile(importer, `---\nimports:\n  - path: ./${target}\n    as: c\n---\nBody\n`);
      const [native] = await compileFileOutcomes('native', [entry]);
      const [wasm] = await compileFileOutcomes('wasm', [entry]);
      if (code === null) {
        assert.deepEqual([wasm, native], [native, { output: '\n\nEntry\n' }], 'the valid target must compile');
      } else {
        const summarize = (err) => [err.code, err.message, err.span?.offset ?? null, err.span?.length ?? null];
        const expected = [code, message, null, null];
        assert.deepEqual([summarize(wasm), summarize(native)], [summarize(native), expected], target);
      }
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('native and WASM compileFile agree at 255–259 total modules in flat and nested graphs', async (t) => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'mds-module-count-parity-'));
  try {
    await writeFile(path.join(root, '.mdsroot'), '');
    const cases = [];
    for (const nested of [false, true]) {
      for (const total of [255, 256, 257, 258, 259]) {
        const label = `${nested ? 'nested' : 'flat'}-${total}`;
        const dir = path.join(root, label);
        await mkdir(dir);
        const imports = (start) => Array.from({ length: total - start }, (_, i) => {
          const id = start + i;
          return `@import "./m${id}.mds" as m${id}\n`;
        }).join('');
        for (let i = 1; i < total; i++) {
          // Two modules stay in flight while the nested graph resolves its leaves.
          const source = nested && i === 1 ? `${imports(2)}Branch\n` : 'Leaf\n';
          await writeFile(path.join(dir, `m${i}.mds`), source);
        }
        const entry = path.join(dir, 'entry.mds');
        await writeFile(entry, `${nested ? '@import "./m1.mds" as m1\n' : imports(1)}OK\n`);
        cases.push({ label, entry, total });
      }
    }
    // The helper forces each public backend and fails if that backend is absent.
    const files = cases.map(({ entry }) => entry);
    const native = await compileFileOutcomes('native', files);
    const wasm = await compileFileOutcomes('wasm', files);
    const sharedMessage = native[cases.findIndex(({ total }) => total === 258)].message;
    assert.equal(typeof sharedMessage, 'string', 'native must refuse the first graph above the cap');
    for (const [i, { label, total }] of cases.entries()) {
      await t.test(label, () => {
        const expected = total <= 257 ? { output: 'OK\n' } : {
          code: 'mds::resource_limit', message: sharedMessage, help: null, span: null,
        };
        assert.deepEqual([wasm[i], native[i]], [native[i], expected], label);
      });
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

// ---------------------------------------------------------------------------
// Method manifest assertions (AC-API-09)
// ---------------------------------------------------------------------------

describe('backend contract — method manifest', () => {
  test('U-BC1: BASE_METHODS contains compile, check, lint', () => {
    assert.deepEqual(
      [...BASE_METHODS].sort(),
      ['check', 'compile', 'lint'],
    );
  });

  test('U-BC2: NODE_METHODS contains compileFile, checkFile, lintFile', () => {
    assert.deepEqual(
      [...NODE_METHODS].sort(),
      ['checkFile', 'compileFile', 'lintFile'],
    );
  });

  test('U-BC3: WASM_EXPORTS contains BASE_METHODS plus lintVirtual, scanImportRecords and preflightModule', () => {
    const expected = [...BASE_METHODS, 'lintVirtual', 'scanImportRecords', 'preflightModule'].sort();
    assert.deepEqual([...WASM_EXPORTS].sort(), expected);
  });

  test('U-BC3a: WASM_EXPORTS does NOT include compileMessages', () => {
    assert.ok(
      !WASM_EXPORTS.includes('compileMessages'),
      'WASM_EXPORTS must not include compileMessages after intrinsic-output refactor',
    );
  });
});

// ---------------------------------------------------------------------------
// validateBackendMethods
// ---------------------------------------------------------------------------

describe('backend contract — validateBackendMethods', () => {
  test('U-BC4: validateBackendMethods passes for a complete stub', () => {
    const stub = {
      compile: () => {},
      check: () => {},
      lint: () => {},
    };
    assert.doesNotThrow(() => validateBackendMethods(stub, BASE_METHODS, 'test stub'));
  });

  test('U-BC5: validateBackendMethods throws when a method is missing', () => {
    const stub = { compile: () => {} }; // missing check and lint
    assert.throws(
      () => validateBackendMethods(stub, BASE_METHODS, 'test stub'),
      (err) => {
        assert.ok(err instanceof Error);
        // First missing method in iteration order; must name the missing method.
        assert.ok(
          err.message.includes('check') || err.message.includes('lint'),
          `error must name a missing method, got: ${err.message}`,
        );
        assert.ok(
          err.message.includes('test stub'),
          `error must name the context, got: ${err.message}`,
        );
        return true;
      },
    );
  });

  test('U-BC6: validateBackendMethods tolerates extra methods beyond the manifest', () => {
    const stub = {
      compile: () => {},
      check: () => {},
      lint: () => {},
      extraMethod: () => {},
    };
    assert.doesNotThrow(() => validateBackendMethods(stub, BASE_METHODS, 'test stub'));
  });

  test('U-BC7: validateBackendMethods throws when a property exists but is not a function', () => {
    const stub = {
      compile: () => {},
      check: 42, // wrong type — missing lint too, but check is found first
    };
    assert.throws(
      () => validateBackendMethods(stub, BASE_METHODS, 'test stub'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(err.message.includes('check'), `expected "check" in message, got: ${err.message}`);
        return true;
      },
    );
  });
});

// ---------------------------------------------------------------------------
// assertResultShape — compile / kind='markdown' (AC-API-10)
// ---------------------------------------------------------------------------

describe('backend contract — assertResultShape compile kind=markdown', () => {
  test('U-BC8: compile kind=markdown — valid result passes', () => {
    assert.doesNotThrow(() =>
      assertResultShape({ kind: 'markdown', output: 'hello', warnings: [], dependencies: [] }, 'compile'),
    );
  });

  test('U-BC8a: compile kind=markdown — valid result with extra fields passes', () => {
    assert.doesNotThrow(() =>
      assertResultShape(
        { kind: 'markdown', output: 'hello', warnings: [], dependencies: [], extra: 'ignored' },
        'compile',
      ),
    );
  });

  test('U-BC8b: compile kind=markdown — wrong-typed output (number) is rejected (AC-API-11)', () => {
    assert.throws(
      () => assertResultShape({ kind: 'markdown', output: 42, warnings: [], dependencies: [] }, 'compile'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(err.message.includes('output'), `expected "output" in error, got: ${err.message}`);
        assert.ok(err.message.includes('compile'), `expected "compile" in error, got: ${err.message}`);
        assert.equal(/** @type {any} */ (err).code, 'mds::invalid_backend_result');
        return true;
      },
    );
  });

  test('U-BC8c: compile kind=markdown — missing dependencies is rejected', () => {
    assert.throws(
      () => assertResultShape({ kind: 'markdown', output: 'hello', warnings: [] }, 'compile'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('dependencies'),
          `expected "dependencies" in error, got: ${err.message}`,
        );
        return true;
      },
    );
  });

  test('U-BC8d: compile kind=markdown — missing warnings is rejected', () => {
    assert.throws(
      () => assertResultShape({ kind: 'markdown', output: 'hello', dependencies: [] }, 'compile'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(err.message.includes('warnings'), `expected "warnings" in error, got: ${err.message}`);
        return true;
      },
    );
  });

  test('U-BC8e: compile kind=markdown — inactive field "messages" present is rejected (AC-API-10)', () => {
    assert.throws(
      () => assertResultShape(
        { kind: 'markdown', output: 'hello', messages: [], warnings: [], dependencies: [] },
        'compile',
      ),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('messages') || err.message.includes('inactive'),
          `expected inactive-field error, got: ${err.message}`,
        );
        assert.equal(/** @type {any} */ (err).code, 'mds::invalid_backend_result');
        return true;
      },
    );
  });
});

// ---------------------------------------------------------------------------
// assertResultShape — compile / kind='messages' (AC-API-10)
// ---------------------------------------------------------------------------

describe('backend contract — assertResultShape compile kind=messages', () => {
  test('U-BC8f: compile kind=messages — valid result passes', () => {
    assert.doesNotThrow(() =>
      assertResultShape(
        { kind: 'messages', messages: [{ role: 'user', content: 'hi' }], warnings: [], dependencies: [] },
        'compile',
      ),
    );
  });

  test('U-BC8g: compile kind=messages — non-array messages is rejected (AC-API-11)', () => {
    assert.throws(
      () => assertResultShape(
        { kind: 'messages', messages: 'not-an-array', warnings: [], dependencies: [] },
        'compile',
      ),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(err.message.includes('messages'));
        assert.equal(/** @type {any} */ (err).code, 'mds::invalid_backend_result');
        return true;
      },
    );
  });

  test('U-BC8h: compile kind=messages — inactive field "output" present is rejected (AC-API-10)', () => {
    assert.throws(
      () => assertResultShape(
        { kind: 'messages', output: 'oops', messages: [], warnings: [], dependencies: [] },
        'compile',
      ),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('output') || err.message.includes('inactive'),
          `expected inactive-field error, got: ${err.message}`,
        );
        assert.equal(/** @type {any} */ (err).code, 'mds::invalid_backend_result');
        return true;
      },
    );
  });

  test('U-BC8i: compile — unknown kind is rejected (AC-API-10)', () => {
    assert.throws(
      () => assertResultShape({ kind: 'unknown', warnings: [], dependencies: [] }, 'compile'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.equal(/** @type {any} */ (err).code, 'mds::invalid_backend_result');
        return true;
      },
    );
  });

  test('U-BC8j: compile — missing kind field is rejected (AC-API-10)', () => {
    assert.throws(
      () => assertResultShape({ output: 'hello', warnings: [], dependencies: [] }, 'compile'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.equal(/** @type {any} */ (err).code, 'mds::invalid_backend_result');
        return true;
      },
    );
  });
});

// ---------------------------------------------------------------------------
// assertResultShape — check
// ---------------------------------------------------------------------------

describe('backend contract — assertResultShape check', () => {
  test('U-BC9: check — valid result passes', () => {
    assert.doesNotThrow(() => assertResultShape({ warnings: [] }, 'check'));
  });

  test('U-BC9a: check — valid result with extra fields passes', () => {
    assert.doesNotThrow(() =>
      assertResultShape({ warnings: [], extra: true }, 'check'),
    );
  });

  test('U-BC9b: check — missing warnings is rejected (AC-API-11)', () => {
    assert.throws(
      () => assertResultShape({}, 'check'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(err.message.includes('warnings'), `expected "warnings" in error, got: ${err.message}`);
        assert.equal(/** @type {any} */ (err).code, 'mds::invalid_backend_result');
        return true;
      },
    );
  });
});

// ---------------------------------------------------------------------------
// assertResultShape — lint
// ---------------------------------------------------------------------------

describe('backend contract — assertResultShape lint', () => {
  test('U-BC9c: lint — valid result passes', () => {
    assert.doesNotThrow(() =>
      assertResultShape({ version: 1, files: [], truncated: false }, 'lint'),
    );
  });

  test('U-BC9d: lint — valid result with file entries and extra fields passes', () => {
    assert.doesNotThrow(() =>
      assertResultShape(
        {
          version: 1,
          files: [{ file: 'foo.mds', diagnostics: [] }],
          truncated: false,
          extra: 'ignored',
        },
        'lint',
      ),
    );
  });

  test('U-BC9e: lint — missing version is rejected', () => {
    assert.throws(
      () => assertResultShape({ files: [], truncated: false }, 'lint'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(err.message.includes('version'), `expected "version" in error, got: ${err.message}`);
        assert.equal(/** @type {any} */ (err).code, 'mds::invalid_backend_result');
        return true;
      },
    );
  });

  test('U-BC9f: lint — non-array files is rejected', () => {
    assert.throws(
      () => assertResultShape({ version: 1, files: 'bad', truncated: false }, 'lint'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(err.message.includes('files'), `expected "files" in error, got: ${err.message}`);
        assert.equal(/** @type {any} */ (err).code, 'mds::invalid_backend_result');
        return true;
      },
    );
  });

  test('U-BC9g: lint — missing truncated is rejected', () => {
    assert.throws(
      () => assertResultShape({ version: 1, files: [] }, 'lint'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(err.message.includes('truncated'), `expected "truncated" in error, got: ${err.message}`);
        assert.equal(/** @type {any} */ (err).code, 'mds::invalid_backend_result');
        return true;
      },
    );
  });
});

// ---------------------------------------------------------------------------
// Performance: O(1) validation — no per-element array traversal (AC-PERF-04)
// ---------------------------------------------------------------------------

describe('backend contract — O(1) array validation (AC-PERF-04)', () => {
  test('U-BC11: assertResultShape does not iterate 10k-element warnings array', () => {
    // Proxy wraps the large array. If assertResultShape accesses any element
    // (numeric index), the counter increments. The requirement is zero accesses.
    let elementAccessCount = 0;
    const bigWarnings = new Proxy(new Array(10_000).fill('w'), {
      get(target, prop) {
        if (typeof prop === 'string' && /^\d+$/.test(prop)) {
          elementAccessCount += 1;
        }
        return Reflect.get(target, prop);
      },
    });

    assertResultShape(
      { kind: 'markdown', output: 'ok', warnings: bigWarnings, dependencies: [] },
      'compile',
    );
    assert.equal(
      elementAccessCount,
      0,
      `assertResultShape must not access array elements; accessed ${elementAccessCount} element(s)`,
    );
  });

  test('U-BC12: assertResultShape does not iterate 10k-element messages array', () => {
    let elementAccessCount = 0;
    const bigMessages = new Proxy(new Array(10_000).fill({ role: 'user', content: 'hi' }), {
      get(target, prop) {
        if (typeof prop === 'string' && /^\d+$/.test(prop)) {
          elementAccessCount += 1;
        }
        return Reflect.get(target, prop);
      },
    });

    assertResultShape(
      { kind: 'messages', messages: bigMessages, warnings: [], dependencies: [] },
      'compile',
    );
    assert.equal(
      elementAccessCount,
      0,
      `assertResultShape must not access array elements; accessed ${elementAccessCount} element(s)`,
    );
  });

  test('U-BC12a: assertResultShape does not iterate 10k-element dependencies array', () => {
    let elementAccessCount = 0;
    const bigDeps = new Proxy(new Array(10_000).fill('/path/dep.mds'), {
      get(target, prop) {
        if (typeof prop === 'string' && /^\d+$/.test(prop)) {
          elementAccessCount += 1;
        }
        return Reflect.get(target, prop);
      },
    });

    assertResultShape(
      { kind: 'markdown', output: 'ok', warnings: [], dependencies: bigDeps },
      'compile',
    );
    assert.equal(
      elementAccessCount,
      0,
      `assertResultShape must not access array elements (deps); accessed ${elementAccessCount} element(s)`,
    );
  });
});

// ---------------------------------------------------------------------------
// Backend stub parity — both backends expose exactly the manifest method set (AC-API-09)
// ---------------------------------------------------------------------------

describe('backend contract — parity: WASM backend exposes BASE_METHODS (AC-API-09)', () => {
  // Build a minimal stub WasmModule for createWasmBackend, which calls only these.
  // Stubs return valid discriminated-union shapes.
  const validMarkdownResult = { kind: 'markdown', output: '', warnings: [], dependencies: [] };
  const validCheckResult = { warnings: [] };
  const validLintResult = { version: 1, files: [], truncated: false };

  const stubWasmModule = {
    compile: () => validMarkdownResult,
    check: () => validCheckResult,
    lint: () => validLintResult,
    lintVirtual: () => validLintResult,
  };

  const wasmBackend = createWasmBackend(stubWasmModule);

  test('U-BC13: WASM backend exposes every BASE_METHODS name as a function (includes lint)', () => {
    for (const method of BASE_METHODS) {
      assert.equal(
        typeof (/** @type {any} */ (wasmBackend))[method],
        'function',
        `WASM backend must have method "${method}"`,
      );
    }
  });

  test('U-BC13a: WASM backend does NOT expose NODE_METHODS (file ops are JS-side via wrapWithFileOps)', () => {
    for (const method of NODE_METHODS) {
      assert.notEqual(
        typeof (/** @type {any} */ (wasmBackend))[method],
        'function',
        `MdsBaseBackend (WASM) must not have file-op "${method}" before wrapWithFileOps`,
      );
    }
  });

  test('U-BC13b: WASM backend does NOT expose compileMessages (AC-API-12)', () => {
    assert.notEqual(
      typeof (/** @type {any} */ (wasmBackend))['compileMessages'],
      'function',
      'WASM backend must not have compileMessages after intrinsic-output refactor',
    );
  });

  test('U-BC13c: WASM backend compile returns valid kind=markdown result from stub', () => {
    const result = wasmBackend.compile('');
    assert.equal(result.kind, 'markdown');
    assertResultShape(result, 'compile');
  });
});

describe('backend contract — parity: native backend exposes BASE_METHODS + NODE_METHODS (AC-API-09)', () => {
  const validMarkdownResult = { kind: 'markdown', output: '', warnings: [], dependencies: [] };
  const validCheckResult = { warnings: [] };
  const validLintResult = { version: 1, files: [], truncated: false };

  const stubAddon = {
    compile: () => validMarkdownResult,
    check: () => validCheckResult,
    compileFile: () => validMarkdownResult,
    checkFile: () => validCheckResult,
    lint: () => validLintResult,
    lintFile: () => validLintResult,
    lintVirtual: () => validLintResult,
  };

  const nativeBackend = createNativeBackend(stubAddon);

  test('U-BC14: native backend exposes every BASE_METHODS name as a function', () => {
    for (const method of BASE_METHODS) {
      assert.equal(
        typeof (/** @type {any} */ (nativeBackend))[method],
        'function',
        `native backend must have method "${method}"`,
      );
    }
  });

  test('U-BC14a: native backend exposes every NODE_METHODS name as a function', () => {
    for (const method of NODE_METHODS) {
      assert.equal(
        typeof (/** @type {any} */ (nativeBackend))[method],
        'function',
        `native backend must have file-op method "${method}"`,
      );
    }
  });

  test('U-BC14b: createNativeBackend throws when a NODE_METHOD is missing from the addon (AC-API-09)', () => {
    const incompleteAddon = {
      compile: () => validMarkdownResult,
      check: () => validCheckResult,
      // missing compileFile and checkFile
    };
    assert.throws(
      () => createNativeBackend(incompleteAddon),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('native addon'),
          `error must mention "native addon", got: ${err.message}`,
        );
        return true;
      },
    );
  });

  test('U-BC14c: native backend check returns valid result from stub', async () => {
    const result = nativeBackend.check('');
    assertResultShape(result, 'check');
  });

  test('U-BC14d: native backend compileFile returns valid result from stub', async () => {
    const result = await nativeBackend.compileFile('path/to/file.mds');
    assertResultShape(result, 'compile');
  });

  test('U-BC14f: native backend does NOT expose compileMessages or compileMessagesFile (AC-API-12)', () => {
    assert.notEqual(
      typeof (/** @type {any} */ (nativeBackend))['compileMessages'],
      'function',
      'native backend must not have compileMessages',
    );
    assert.notEqual(
      typeof (/** @type {any} */ (nativeBackend))['compileMessagesFile'],
      'function',
      'native backend must not have compileMessagesFile',
    );
  });
});
