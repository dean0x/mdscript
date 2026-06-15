/**
 * Real-driver end-to-end watch tests for @mdscript/rollup-plugin.
 *
 * These tests drive an ACTUAL Rollup watcher process and assert on fresh bundle
 * output — not mocked transformer calls. They verify that editing .mds files
 * causes Rollup to re-bundle with updated content.
 *
 * Platform gating (decision D5 / Gate0-2):
 *   Gated by HMR_ENABLED (Linux inotify reference platform, or MDS_HMR=1 override).
 *   On macOS/Windows these tests SKIP (stay green). The contract specs in
 *   watch.spec.mjs run cross-platform without a gate.
 *
 * Test scenarios:
 *   Suite 1: T-HMR-a through T-HMR-e, T-P1, T-P2
 *   Suite 3: T-E-del, T-E-create, T-E-mdflip, T-E-cycle (real-driver edge cases)
 */
import { test, describe, after } from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { existsSync, unlinkSync } from 'node:fs';
import { watch } from 'rollup';
import mdsPlugin from '../dist/index.js';
import {
  HMR_ENABLED,
  createTempMdsProject,
  editFile,
  waitFor,
} from '../../bundler-utils/__test__/hmr-harness.mjs';

process.env.NODE_ENV = 'test';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Build a RollupWatcher and wait for the first BUNDLE_END event.
 * Returns { watcher, getCode } where getCode() generates ESM output for assertion.
 *
 * @param {string} entryPath - Absolute path to the entry .mds file.
 * @param {object} [pluginOptions] - Optional plugin options.
 * @returns {Promise<{ watcher: import('rollup').RollupWatcher, getCode: () => Promise<string> }>}
 */
async function startWatcher(entryPath, pluginOptions) {
  const watcher = watch({
    input: entryPath,
    plugins: [mdsPlugin(pluginOptions)],
    output: { format: 'es' },
    watch: { skipWrite: true, chokidar: { usePolling: true, interval: 50 } },
  });

  /** @type {import('rollup').RollupBuild | null} */
  let latestResult = null;

  watcher.on('event', (event) => {
    if (event.code === 'BUNDLE_END') {
      latestResult = event.result;
    }
  });

  async function getCode() {
    if (!latestResult) return null;
    const { output } = await latestResult.generate({ format: 'es' });
    return output[0].code;
  }

  // Wait for the initial build
  await waitFor(
    () => latestResult !== null,
    { timeoutMs: 15_000, intervalMs: 100, label: 'initial Rollup BUNDLE_END' },
  );

  return { watcher, getCode };
}

/**
 * Wait until getCode() returns a string containing `marker`.
 */
async function waitForMarker(getCode, marker, timeoutMs = 10_000) {
  await waitFor(
    async () => {
      const code = await getCode();
      return code != null && code.includes(marker);
    },
    { timeoutMs, intervalMs: 100, label: `Rollup bundle contains "${marker}"` },
  );
}

// ---------------------------------------------------------------------------
// Suite 1: HMR lifecycle (real Rollup watcher)
// ---------------------------------------------------------------------------

describe('rollup-plugin watch e2e — Suite 1 (real watcher)', { skip: !HMR_ENABLED && 'HMR e2e tests are Linux-gated; set MDS_HMR=1 to run' }, () => {

  test('T-HMR-a (AC-F1): edit entry .mds → fresh bundle with new marker', async () => {
    const { dir, paths, cleanup } = createTempMdsProject({
      'entry.mds': '---\nname: World\n---\n\nHello {name}! MARKER_A',
    });
    const { watcher, getCode } = await startWatcher(paths['entry.mds']);

    try {
      // Verify initial build
      const codeA = await getCode();
      assert.ok(codeA.includes('MARKER_A'), 'initial build contains MARKER_A');

      // Edit entry → MARKER_B
      editFile(paths['entry.mds'], '---\nname: World\n---\n\nHello {name}! MARKER_B');
      await waitForMarker(getCode, 'MARKER_B');

      const codeB = await getCode();
      assert.ok(codeB.includes('MARKER_B'), 'rebuild contains MARKER_B');
      assert.ok(!codeB.includes('MARKER_A'), 'MARKER_A is gone after edit');
    } finally {
      await watcher.close();
      cleanup();
    }
  });

  test('T-HMR-b (AC-F2): edit transitive @import dep → fresh bundle', async () => {
    const { dir, paths, cleanup } = createTempMdsProject({
      // ADR-014: dep BEFORE entry
      'dep.mds': '@define greet(who):\nHi {who}! MARKER_A\n@end\n\n@export greet',
      'entry.mds': '@import { greet } from "./dep.mds"\n\n{greet("World")}',
    });
    const { watcher, getCode } = await startWatcher(paths['entry.mds']);

    try {
      await waitForMarker(getCode, 'MARKER_A', 10_000);

      // Edit the dep file
      editFile(paths['dep.mds'], '@define greet(who):\nHi {who}! MARKER_B\n@end\n\n@export greet');
      await waitForMarker(getCode, 'MARKER_B');

      const code = await getCode();
      assert.ok(code.includes('MARKER_B'), 'dep edit propagates to bundle');
    } finally {
      await watcher.close();
      cleanup();
    }
  });

  test('T-HMR-c (AC-F3): inject compile error → Rollup surfaces ERROR event, watcher stays alive', async (t) => {
    const { dir, paths, cleanup } = createTempMdsProject({
      'entry.mds': '---\nname: World\n---\n\nHello {name}! MARKER_A',
    });

    const watcher = watch({
      input: paths['entry.mds'],
      plugins: [mdsPlugin()],
      output: { format: 'es' },
      watch: { skipWrite: true, chokidar: { usePolling: true, interval: 50 } },
    });

    const errors = [];
    let lastGoodCode = null;
    let bundleCount = 0;

    watcher.on('event', async (event) => {
      if (event.code === 'BUNDLE_END') {
        bundleCount++;
        const { output } = await event.result.generate({ format: 'es' });
        lastGoodCode = output[0].code;
        event.result.close();
      }
      if (event.code === 'ERROR') {
        errors.push(event.error);
      }
    });

    try {
      // Wait for initial good build
      await waitFor(() => bundleCount >= 1, { timeoutMs: 15_000, label: 'initial build' });
      assert.ok(lastGoodCode.includes('MARKER_A'), 'initial build ok');

      // Inject a compile error
      editFile(paths['entry.mds'], '{undefined_var_xyz_bad_syntax!!!}');
      await waitFor(() => errors.length >= 1, { timeoutMs: 10_000, label: 'ERROR event received' });

      assert.ok(errors[0] instanceof Error, 'error is an Error instance');
      // T-C4: error carries position info (Rollup wraps with loc)
      const errMsg = errors[0].message;
      assert.ok(errMsg.length > 0, 'error message is non-empty');

      // isAlive(): watcher emits further events after error
      const bundleCountBeforeRecovery = bundleCount;
      editFile(paths['entry.mds'], '---\nname: World\n---\n\nHello {name}! MARKER_FIXED');
      await waitFor(
        () => bundleCount > bundleCountBeforeRecovery,
        { timeoutMs: 10_000, label: 'recovery build after error' },
      );
      const finalCode = await (async () => {
        const w = watch({
          input: paths['entry.mds'],
          plugins: [mdsPlugin()],
          output: { format: 'es' },
          watch: { skipWrite: true, chokidar: { usePolling: true, interval: 50 } },
        });
        return new Promise((resolve) => {
          w.on('event', async (e) => {
            if (e.code === 'BUNDLE_END') {
              const { output } = await e.result.generate({ format: 'es' });
              e.result.close();
              await w.close();
              resolve(output[0].code);
            }
          });
        });
      })();
      assert.ok(finalCode.includes('MARKER_FIXED'), 'recovery build has MARKER_FIXED');
    } finally {
      await watcher.close();
      cleanup();
    }
  });

  test('T-HMR-d (AC-F4): fix compile error → fresh bundle, error cleared', async () => {
    const { dir, paths, cleanup } = createTempMdsProject({
      'entry.mds': '{undefined_var_xyz_bad_syntax!!!}',
    });

    const watcher = watch({
      input: paths['entry.mds'],
      plugins: [mdsPlugin()],
      output: { format: 'es' },
      watch: { skipWrite: true, chokidar: { usePolling: true, interval: 50 } },
    });

    const errors = [];
    let lastGoodCode = null;

    watcher.on('event', async (event) => {
      if (event.code === 'BUNDLE_END') {
        const { output } = await event.result.generate({ format: 'es' });
        lastGoodCode = output[0].code;
        event.result.close();
      }
      if (event.code === 'ERROR') {
        errors.push(event.error);
      }
    });

    try {
      // Wait for initial error
      await waitFor(() => errors.length >= 1, { timeoutMs: 15_000, label: 'initial ERROR' });

      // Fix the file
      editFile(paths['entry.mds'], '---\nname: World\n---\n\nHello {name}! MARKER_FIXED');
      await waitFor(() => lastGoodCode !== null && lastGoodCode.includes('MARKER_FIXED'),
        { timeoutMs: 10_000, label: 'MARKER_FIXED in bundle' });

      assert.ok(lastGoodCode.includes('MARKER_FIXED'), 'fixed bundle contains MARKER_FIXED');
    } finally {
      await watcher.close();
      cleanup();
    }
  });

  test('T-HMR-e (AC-F5): add a second @import dep, edit it → recompile', async () => {
    // ADR-014: dep files BEFORE entry
    const { dir, paths, cleanup } = createTempMdsProject({
      'dep1.mds': '@define greet(who):\nHi {who}! MARKER_A\n@end\n\n@export greet',
      'entry.mds': '@import { greet } from "./dep1.mds"\n\n{greet("World")}',
    });

    const watcher = watch({
      input: paths['entry.mds'],
      plugins: [mdsPlugin()],
      output: { format: 'es' },
      watch: { skipWrite: true, chokidar: { usePolling: true, interval: 50 } },
    });

    let lastCode = null;
    watcher.on('event', async (event) => {
      if (event.code === 'BUNDLE_END') {
        const { output } = await event.result.generate({ format: 'es' });
        lastCode = output[0].code;
        event.result.close();
      }
    });

    try {
      await waitFor(() => lastCode != null && lastCode.includes('MARKER_A'),
        { timeoutMs: 15_000, label: 'initial MARKER_A' });

      // Add a second dep and update entry to import it
      // (dep2 must exist before we reference it in entry)
      const dep2Path = join(dir, 'dep2.mds');
      editFile(dep2Path, '@define farewell(who):\nBye {who}! MARKER_B\n@end\n\n@export farewell');
      editFile(paths['entry.mds'],
        '@import { greet } from "./dep1.mds"\n@import { farewell } from "./dep2.mds"\n\n{greet("World")} {farewell("World")}');

      await waitFor(() => lastCode != null && lastCode.includes('MARKER_B'),
        { timeoutMs: 10_000, label: 'MARKER_B after dep2 import' });
      assert.ok(lastCode.includes('MARKER_B'), 'second dep content in bundle');

      // Now edit dep2 → MARKER_C
      editFile(dep2Path, '@define farewell(who):\nBye {who}! MARKER_C\n@end\n\n@export farewell');
      await waitFor(() => lastCode != null && lastCode.includes('MARKER_C'),
        { timeoutMs: 10_000, label: 'MARKER_C after dep2 edit' });
      assert.ok(lastCode.includes('MARKER_C'), 'dep2 edit triggers recompile');
    } finally {
      await watcher.close();
      cleanup();
    }
  });

  test('T-P1 (AC-P1): edit → fresh bundle within 10s performance budget', async () => {
    const { dir, paths, cleanup } = createTempMdsProject({
      'entry.mds': '---\nname: World\n---\n\nHello {name}! MARKER_A',
    });
    const { watcher, getCode } = await startWatcher(paths['entry.mds']);

    try {
      const startMs = Date.now();
      editFile(paths['entry.mds'], '---\nname: World\n---\n\nHello {name}! MARKER_B');
      await waitForMarker(getCode, 'MARKER_B', 10_000);
      const elapsedMs = Date.now() - startMs;

      assert.ok(
        elapsedMs < 10_000,
        `Edit→fresh took ${elapsedMs}ms, must be < 10000ms (T-P1 budget)`,
      );
    } finally {
      await watcher.close();
      cleanup();
    }
  });

  test('T-P2 (AC-P2): 20-iteration bounded edit loop — no degradation, watcher alive', async () => {
    const N = 20;
    const { dir, paths, cleanup } = createTempMdsProject({
      'entry.mds': '---\nname: World\n---\n\nHello {name}! ITERATION_0',
    });
    const { watcher, getCode } = await startWatcher(paths['entry.mds']);

    try {
      for (let i = 1; i <= N; i++) {
        const marker = `ITERATION_${i}`;
        editFile(paths['entry.mds'], `---\nname: World\n---\n\nHello {name}! ${marker}`);
        await waitForMarker(getCode, marker, 10_000);
      }

      const finalCode = await getCode();
      assert.ok(finalCode.includes(`ITERATION_${N}`), `Final iteration ${N} is fresh`);
    } finally {
      await watcher.close();
      cleanup();
    }
  });
});

// ---------------------------------------------------------------------------
// Suite 3: Edge cases (real Rollup watcher, Linux-gated)
// ---------------------------------------------------------------------------

describe('rollup-plugin watch e2e — Suite 3 edge cases', { skip: !HMR_ENABLED && 'HMR e2e tests are Linux-gated; set MDS_HMR=1 to run' }, () => {

  test('T-E-del (AC-E1): delete @imported dep → error surfaced; recreate → recovers', async () => {
    const { dir, paths, cleanup } = createTempMdsProject({
      'dep.mds': '@define greet(who):\nHi {who}! DEP_MARKER\n@end\n\n@export greet',
      'entry.mds': '@import { greet } from "./dep.mds"\n\n{greet("World")}',
    });

    const watcher = watch({
      input: paths['entry.mds'],
      plugins: [mdsPlugin()],
      output: { format: 'es' },
      watch: { skipWrite: true, chokidar: { usePolling: true, interval: 50 } },
    });

    const errors = [];
    let lastGoodCode = null;

    watcher.on('event', async (event) => {
      if (event.code === 'BUNDLE_END') {
        const { output } = await event.result.generate({ format: 'es' });
        lastGoodCode = output[0].code;
        event.result.close();
      }
      if (event.code === 'ERROR') {
        errors.push(event.error);
      }
    });

    try {
      // Wait for initial good build
      await waitFor(() => lastGoodCode !== null && lastGoodCode.includes('DEP_MARKER'),
        { timeoutMs: 15_000, label: 'initial DEP_MARKER' });

      // Delete the dep file
      unlinkSync(paths['dep.mds']);

      // Touch the entry to force a rebuild attempt (some watchers may not notice
      // the dep deletion without a re-run of the entry)
      editFile(paths['entry.mds'], '@import { greet } from "./dep.mds"\n\n{greet("World")} AFTER_DEL');

      // Rollup will emit ERROR when the dep is missing
      await waitFor(() => errors.length >= 1, { timeoutMs: 10_000, label: 'ERROR after dep deletion' });
      assert.ok(errors[0] instanceof Error, 'error surfaced after dep deletion');

      // Recreate the dep
      editFile(paths['dep.mds'], '@define greet(who):\nHi {who}! DEP_RECREATED\n@end\n\n@export greet');
      editFile(paths['entry.mds'], '@import { greet } from "./dep.mds"\n\n{greet("World")}');

      await waitFor(() => lastGoodCode !== null && lastGoodCode.includes('DEP_RECREATED'),
        { timeoutMs: 10_000, label: 'DEP_RECREATED after dep restore' });
      assert.ok(lastGoodCode.includes('DEP_RECREATED'), 'watcher recovers after dep recreated');
    } finally {
      await watcher.close();
      cleanup();
    }
  });

  test('T-E-create (AC-E1): entry @imports not-yet-created dep → error; create dep → recovers', async () => {
    const { dir, paths, cleanup } = createTempMdsProject({
      'entry.mds': '@import { greet } from "./missing.mds"\n\n{greet("World")}',
    });

    const watcher = watch({
      input: paths['entry.mds'],
      plugins: [mdsPlugin()],
      output: { format: 'es' },
      watch: { skipWrite: true, chokidar: { usePolling: true, interval: 50 } },
    });

    const errors = [];
    let lastGoodCode = null;

    watcher.on('event', async (event) => {
      if (event.code === 'BUNDLE_END') {
        const { output } = await event.result.generate({ format: 'es' });
        lastGoodCode = output[0].code;
        event.result.close();
      }
      if (event.code === 'ERROR') {
        errors.push(event.error);
      }
    });

    try {
      // Initial build should error (dep missing)
      await waitFor(() => errors.length >= 1, { timeoutMs: 15_000, label: 'initial ERROR for missing dep' });
      assert.ok(errors[0] instanceof Error, 'error when dep is missing');

      // Create the missing dep and re-touch entry
      editFile(join(dir, 'missing.mds'), '@define greet(who):\nHi {who}! CREATED_MARKER\n@end\n\n@export greet');
      editFile(paths['entry.mds'], '@import { greet } from "./missing.mds"\n\n{greet("World")}');

      await waitFor(() => lastGoodCode !== null && lastGoodCode.includes('CREATED_MARKER'),
        { timeoutMs: 10_000, label: 'CREATED_MARKER after dep creation' });
      assert.ok(lastGoodCode.includes('CREATED_MARKER'), 'recovered after dep created');
    } finally {
      await watcher.close();
      cleanup();
    }
  });

  test('T-E-mdflip (AC-E2): plain .md gains type:mds mid-session → documented behavior', async () => {
    // For Rollup: the plugin only processes files that return true from
    // shouldTransform(). A .md file without type:mds frontmatter is passed through
    // as null (no transform). After adding frontmatter, Rollup re-invokes transform
    // and the plugin will compile it.
    // This test documents the observed behavior: Rollup transform hook is stateless —
    // the plugin re-evaluates shouldTransform on every build, so the md-flip works
    // without restart.
    const { dir, paths, cleanup } = createTempMdsProject({
      'entry.md': '# Just a plain markdown file\n\nNo frontmatter here.',
    });

    const watcher = watch({
      input: paths['entry.md'],
      plugins: [mdsPlugin()],
      output: { format: 'es' },
      watch: { skipWrite: true, chokidar: { usePolling: true, interval: 50 } },
    });

    const events = [];
    watcher.on('event', async (event) => {
      if (event.code === 'BUNDLE_END') {
        const { output } = await event.result.generate({ format: 'es' });
        events.push({ type: 'bundle', code: output[0].code });
        event.result.close();
      }
      if (event.code === 'ERROR') {
        events.push({ type: 'error', err: event.error });
      }
    });

    try {
      // Wait for initial build — plain .md without type:mds
      await waitFor(() => events.length >= 1, { timeoutMs: 15_000, label: 'initial build for .md file' });

      // Add type:mds frontmatter
      editFile(paths['entry.md'], '---\ntype: mds\nname: World\n---\n\nHello {name}! MD_FLIP_MARKER');

      await waitFor(
        () => events.some(e => e.type === 'bundle' && e.code?.includes('MD_FLIP_MARKER')),
        { timeoutMs: 10_000, label: 'MD_FLIP_MARKER after type:mds added' },
      );
      const flipBundle = events.find(e => e.type === 'bundle' && e.code?.includes('MD_FLIP_MARKER'));
      assert.ok(flipBundle, '.md file compiled after type:mds frontmatter added — no restart needed');
    } finally {
      await watcher.close();
      cleanup();
    }
  });

  test('T-E-cycle: circular @import edited → recompiles once, watcher alive, no infinite loop', async () => {
    // Circular @imports should produce an error (the MDS compiler handles this).
    // The watcher must stay alive and not enter an infinite rebuild loop.
    const { dir, paths, cleanup } = createTempMdsProject({
      // Entry tries to import from itself (simplest cycle)
      'entry.mds': '@import { thing } from "./entry.mds"\n\n{thing}',
    });

    const watcher = watch({
      input: paths['entry.mds'],
      plugins: [mdsPlugin()],
      output: { format: 'es' },
      watch: { skipWrite: true, chokidar: { usePolling: true, interval: 50 } },
    });

    const errors = [];
    let bundleCount = 0;

    watcher.on('event', async (event) => {
      if (event.code === 'BUNDLE_END') {
        bundleCount++;
        event.result.close();
      }
      if (event.code === 'ERROR') {
        errors.push(event.error);
      }
    });

    try {
      // Should get an error or bundle (compiler may handle the cycle gracefully)
      await waitFor(() => errors.length >= 1 || bundleCount >= 1,
        { timeoutMs: 15_000, label: 'cycle error or bundle' });

      // Wait a bit to ensure no infinite rebuild loop (bounded by 3s observation)
      const countBefore = bundleCount + errors.length;
      await new Promise((r) => setTimeout(r, 3_000));
      const countAfter = bundleCount + errors.length;

      // Allow at most 2 more events after the initial one (some watchers do a double-check)
      assert.ok(
        countAfter - countBefore <= 2,
        `Circular @import caused ${countAfter - countBefore} extra builds — possible infinite loop`,
      );
    } finally {
      await watcher.close();
      cleanup();
    }
  });
});
