/**
 * Backend contract parity tests for @mdscript/mds.
 * Tests: U-BC1 through U-BC13
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
// Method manifest assertions
// ---------------------------------------------------------------------------

describe('backend contract — method manifest', () => {
  test('U-BC1: BASE_METHODS contains compile, check, compileMessages', () => {
    assert.deepEqual(
      [...BASE_METHODS].sort(),
      ['check', 'compile', 'compileMessages'],
    );
  });

  test('U-BC2: NODE_METHODS contains compileFile, checkFile, compileMessagesFile', () => {
    assert.deepEqual(
      [...NODE_METHODS].sort(),
      ['checkFile', 'compileFile', 'compileMessagesFile'],
    );
  });

  test('U-BC3: WASM_EXPORTS contains BASE_METHODS plus scanImports', () => {
    const expected = [...BASE_METHODS, 'scanImports'].sort();
    assert.deepEqual([...WASM_EXPORTS].sort(), expected);
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
      compileMessages: () => {},
    };
    assert.doesNotThrow(() => validateBackendMethods(stub, BASE_METHODS, 'test stub'));
  });

  test('U-BC5: validateBackendMethods throws when a method is missing', () => {
    const stub = { compile: () => {}, check: () => {} }; // missing compileMessages
    assert.throws(
      () => validateBackendMethods(stub, BASE_METHODS, 'test stub'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(
          err.message.includes('compileMessages'),
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
      compileMessages: () => {},
      extraMethod: () => {},
    };
    assert.doesNotThrow(() => validateBackendMethods(stub, BASE_METHODS, 'test stub'));
  });

  test('U-BC7: validateBackendMethods throws when a property exists but is not a function', () => {
    const stub = {
      compile: () => {},
      check: 42, // wrong type
      compileMessages: () => {},
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
// assertResultShape — compile
// ---------------------------------------------------------------------------

describe('backend contract — assertResultShape compile', () => {
  test('U-BC8: compile — valid result passes', () => {
    assert.doesNotThrow(() =>
      assertResultShape({ output: 'hello', warnings: [], dependencies: [] }, 'compile'),
    );
  });

  test('U-BC8a: compile — valid result with extra fields passes', () => {
    assert.doesNotThrow(() =>
      assertResultShape(
        { output: 'hello', warnings: [], dependencies: [], extra: 'ignored' },
        'compile',
      ),
    );
  });

  test('U-BC8b: compile — wrong-typed output (number) is rejected', () => {
    assert.throws(
      () => assertResultShape({ output: 42, warnings: [], dependencies: [] }, 'compile'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(err.message.includes('output'), `expected "output" in error, got: ${err.message}`);
        assert.ok(err.message.includes('compile'), `expected "compile" in error, got: ${err.message}`);
        assert.equal(/** @type {any} */ (err).code, 'mds::invalid_backend_result');
        return true;
      },
    );
  });

  test('U-BC8c: compile — missing dependencies is rejected', () => {
    assert.throws(
      () => assertResultShape({ output: 'hello', warnings: [] }, 'compile'),
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

  test('U-BC8d: compile — missing warnings is rejected', () => {
    assert.throws(
      () => assertResultShape({ output: 'hello', dependencies: [] }, 'compile'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(err.message.includes('warnings'), `expected "warnings" in error, got: ${err.message}`);
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

  test('U-BC9b: check — missing warnings is rejected', () => {
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
// assertResultShape — compileMessages
// ---------------------------------------------------------------------------

describe('backend contract — assertResultShape compileMessages', () => {
  test('U-BC10: compileMessages — valid result passes', () => {
    assert.doesNotThrow(() =>
      assertResultShape({ messages: [], warnings: [], dependencies: [] }, 'compileMessages'),
    );
  });

  test('U-BC10a: compileMessages — valid result with extra fields passes', () => {
    assert.doesNotThrow(() =>
      assertResultShape(
        { messages: [], warnings: [], dependencies: [], meta: null },
        'compileMessages',
      ),
    );
  });

  test('U-BC10b: compileMessages — missing messages is rejected', () => {
    assert.throws(
      () => assertResultShape({ warnings: [], dependencies: [] }, 'compileMessages'),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(err.message.includes('messages'), `expected "messages" in error, got: ${err.message}`);
        assert.equal(/** @type {any} */ (err).code, 'mds::invalid_backend_result');
        return true;
      },
    );
  });

  test('U-BC10c: compileMessages — non-array messages is rejected', () => {
    assert.throws(
      () =>
        assertResultShape(
          { messages: 'not-an-array', warnings: [], dependencies: [] },
          'compileMessages',
        ),
      (err) => {
        assert.ok(err instanceof Error);
        assert.ok(err.message.includes('messages'));
        return true;
      },
    );
  });
});

// ---------------------------------------------------------------------------
// Performance: O(1) validation — no per-element array traversal
// ---------------------------------------------------------------------------

describe('backend contract — O(1) array validation', () => {
  test('U-BC11: assertResultShape does not iterate array elements (10k-element warnings array)', () => {
    // Build a large warnings array. If assertResultShape iterates elements,
    // this test would measurably slow down (and a proxy would catch element access).
    // We use a Proxy to confirm no element index access occurs.
    let elementAccessCount = 0;
    const bigWarnings = new Proxy(new Array(10_000).fill('w'), {
      get(target, prop) {
        // Allow Array.isArray, length, and prototype methods.
        // Flag any numeric index access (element iteration).
        if (typeof prop === 'string' && /^\d+$/.test(prop)) {
          elementAccessCount += 1;
        }
        return Reflect.get(target, prop);
      },
    });

    assertResultShape({ output: 'ok', warnings: bigWarnings, dependencies: [] }, 'compile');
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

    assertResultShape({ messages: bigMessages, warnings: [], dependencies: [] }, 'compileMessages');
    assert.equal(
      elementAccessCount,
      0,
      `assertResultShape must not access array elements; accessed ${elementAccessCount} element(s)`,
    );
  });
});

// ---------------------------------------------------------------------------
// Backend stub parity — both backends expose exactly the manifest method set
// ---------------------------------------------------------------------------

describe('backend contract — parity: WASM backend exposes BASE_METHODS', () => {
  // Build a minimal stub WasmModule that satisfies validateWasmShape.
  // Stubs return valid result shapes so createWasmBackend's per-call validation passes.
  const validCompileResult = { output: '', warnings: [], dependencies: [] };
  const validCheckResult = { warnings: [] };
  const validCompileMessagesResult = { messages: [], warnings: [], dependencies: [] };

  const stubWasmModule = {
    compile: () => validCompileResult,
    check: () => validCheckResult,
    compileMessages: () => validCompileMessagesResult,
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

  test('U-BC13b: WASM backend compile returns valid result from stub', () => {
    const result = wasmBackend.compile('');
    assertResultShape(result, 'compile');
  });
});

describe('backend contract — parity: native backend exposes BASE_METHODS + NODE_METHODS', () => {
  const validCompileResult = { output: '', warnings: [], dependencies: [] };
  const validCheckResult = { warnings: [] };
  const validCompileMessagesResult = { messages: [], warnings: [], dependencies: [] };

  const stubAddon = {
    compile: () => validCompileResult,
    check: () => validCheckResult,
    compileMessages: () => validCompileMessagesResult,
    compileFile: () => validCompileResult,
    checkFile: () => validCheckResult,
    compileMessagesFile: () => validCompileMessagesResult,
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

  test('U-BC14b: createNativeBackend throws when a NODE_METHOD is missing from the addon', () => {
    const incompleteAddon = {
      compile: () => validCompileResult,
      check: () => validCheckResult,
      compileMessages: () => validCompileMessagesResult,
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

  test('U-BC14e: native backend compileMessagesFile returns valid result from stub (PR-A2)', async () => {
    const result = await nativeBackend.compileMessagesFile('path/to/chat.mds');
    assertResultShape(result, 'compileMessages');
  });
});
