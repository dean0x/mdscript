/**
 * Backend contract parity tests for @mdscript/mds.
 * Tests: U-BC1 through U-BC20
 *
 * All tests use STUB backend objects — no live wasm build required.
 * This file is env-independent and must pass in CI on every platform.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import {
  assertResultShape,
  validateBackendMethods,
  BASE_METHODS,
  NODE_METHODS,
  WASM_EXPORTS,
} from '../dist/backend/contract.js';
import { createWasmBackend } from '../dist/backend/wasm.js';
import { createNativeBackend } from '../dist/backend/native.js';

// ---------------------------------------------------------------------------
// Method manifest assertions (AC-API-09)
// ---------------------------------------------------------------------------

describe('backend contract — method manifest', () => {
  test('U-BC1: BASE_METHODS contains compile, check (no compileMessages)', () => {
    assert.deepEqual(
      [...BASE_METHODS].sort(),
      ['check', 'compile'],
    );
  });

  test('U-BC2: NODE_METHODS contains compileFile, checkFile (no compileMessagesFile)', () => {
    assert.deepEqual(
      [...NODE_METHODS].sort(),
      ['checkFile', 'compileFile'],
    );
  });

  test('U-BC3: WASM_EXPORTS contains BASE_METHODS plus scanImports', () => {
    const expected = [...BASE_METHODS, 'scanImports'].sort();
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
    };
    assert.doesNotThrow(() => validateBackendMethods(stub, BASE_METHODS, 'test stub'));
  });

  test('U-BC5: validateBackendMethods throws when a method is missing', () => {
    const stub = { compile: () => {} }; // missing check
    assert.throws(
      () => validateBackendMethods(stub, BASE_METHODS, 'test stub'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('check'),
          `error must name the missing method, got: ${err.message}`,
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
      extraMethod: () => {},
    };
    assert.doesNotThrow(() => validateBackendMethods(stub, BASE_METHODS, 'test stub'));
  });

  test('U-BC7: validateBackendMethods throws when a property exists but is not a function', () => {
    const stub = {
      compile: () => {},
      check: 42, // wrong type
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
  // Build a minimal stub WasmModule that satisfies validateWasmShape.
  // Stubs return valid discriminated-union shapes.
  const validMarkdownResult = { kind: 'markdown', output: '', warnings: [], dependencies: [] };
  const validCheckResult = { warnings: [] };

  const stubWasmModule = {
    compile: () => validMarkdownResult,
    check: () => validCheckResult,
    scanImports: () => [],
  };

  const wasmBackend = createWasmBackend(stubWasmModule);

  test('U-BC13: WASM backend exposes every BASE_METHODS name as a function', () => {
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

  const stubAddon = {
    compile: () => validMarkdownResult,
    check: () => validCheckResult,
    compileFile: () => validMarkdownResult,
    checkFile: () => validCheckResult,
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
