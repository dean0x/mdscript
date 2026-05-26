/**
 * Tests for LazyInit<T>.
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { LazyInit } from '../dist/index.js';

describe('LazyInit', () => {
  test('get() calls factory exactly once across multiple calls', async () => {
    let callCount = 0;
    const lazy = new LazyInit(async () => {
      callCount++;
      return 'value';
    });

    const r1 = await lazy.get();
    const r2 = await lazy.get();
    const r3 = await lazy.get();

    assert.equal(r1, 'value');
    assert.equal(r2, 'value');
    assert.equal(r3, 'value');
    assert.equal(callCount, 1, 'factory should be called exactly once');
  });

  test('concurrent get() calls deduplicate — factory called once under race', async () => {
    let callCount = 0;
    const lazy = new LazyInit(async () => {
      callCount++;
      return 'concurrent-value';
    });

    const [r1, r2, r3] = await Promise.all([
      lazy.get(),
      lazy.get(),
      lazy.get(),
    ]);

    assert.equal(r1, 'concurrent-value');
    assert.equal(r2, 'concurrent-value');
    assert.equal(r3, 'concurrent-value');
    assert.equal(callCount, 1, 'factory should be called once even under concurrent load');
  });

  test('factory rejection clears pending, next get() retries', async () => {
    let callCount = 0;
    const lazy = new LazyInit(async () => {
      callCount++;
      if (callCount === 1) throw new Error('transient failure');
      return 'retry-value';
    });

    // First call — factory rejects.
    await assert.rejects(() => lazy.get(), /transient failure/);

    // Second call — must retry factory, not re-throw the cached rejection.
    const result = await lazy.get();
    assert.equal(result, 'retry-value');
    assert.equal(callCount, 2, 'factory should have been called twice');
  });

  test('reset() during in-flight get() — stale resolution does not corrupt state', async () => {
    let resolve;
    const controlled = new Promise((r) => { resolve = r; });

    let callCount = 0;
    const lazy = new LazyInit(async () => {
      callCount++;
      if (callCount === 1) {
        // First factory: wait for external resolution.
        return controlled;
      }
      return 'fresh-value';
    });

    // Start the first get() — in-flight, awaiting controlled promise.
    const firstGet = lazy.get();

    // Reset mid-flight: generation advances, state is cleared.
    lazy.reset();

    // Resolve the stale factory promise with a value.
    resolve('stale-value');

    // The stale .then() handler must not store 'stale-value'.
    // Awaiting firstGet returns 'stale-value' (the old promise chain propagates
    // the resolved value regardless), but resolved/pending must remain cleared.
    await firstGet;

    // A fresh get() must invoke the factory again and return 'fresh-value'.
    const freshResult = await lazy.get();
    assert.equal(freshResult, 'fresh-value', 'stale factory result must not persist after reset');
    assert.equal(callCount, 2, 'factory must be called again after reset');
  });

  test('reset() clears state, next get() re-invokes factory', async () => {
    let callCount = 0;
    const lazy = new LazyInit(async () => {
      callCount++;
      return `call-${callCount}`;
    });

    const r1 = await lazy.get();
    assert.equal(r1, 'call-1');
    assert.equal(callCount, 1);

    lazy.reset();

    const r2 = await lazy.get();
    assert.equal(r2, 'call-2');
    assert.equal(callCount, 2, 'factory should be called again after reset');
  });

  test('T = void factory works correctly (resolved flag handles undefined return)', async () => {
    let callCount = 0;
    const lazy = new LazyInit(async () => {
      callCount++;
      // Returns void (undefined).
    });

    await lazy.get();
    await lazy.get();

    assert.equal(callCount, 1, 'void factory should be called exactly once');
  });

  test('concurrent get() calls all reject when factory rejects', async () => {
    let callCount = 0;
    const lazy = new LazyInit(async () => {
      callCount++;
      throw new Error('factory-rejection');
    });

    const results = await Promise.allSettled([
      lazy.get(),
      lazy.get(),
      lazy.get(),
    ]);

    assert.equal(callCount, 1, 'factory should be called once even when all concurrent callers reject');
    for (const result of results) {
      assert.equal(result.status, 'rejected', 'all concurrent callers should receive the rejection');
      assert.match(result.reason.message, /factory-rejection/);
    }
  });

  test('factory that returns null works correctly (null is valid T value)', async () => {
    let callCount = 0;
    const lazy = new LazyInit(async () => {
      callCount++;
      return null;
    });

    const r1 = await lazy.get();
    const r2 = await lazy.get();

    assert.equal(r1, null);
    assert.equal(r2, null);
    assert.equal(callCount, 1, 'null-returning factory should be called exactly once');
  });
});
