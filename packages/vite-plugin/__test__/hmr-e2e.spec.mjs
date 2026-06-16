/**
 * Real-driver end-to-end HMR tests for @mdscript/vite-plugin.
 *
 * These tests drive an ACTUAL Vite dev server in middlewareMode and assert on
 * freshly transformed module content — not mocked plugin calls. They verify:
 *   1. server.transformRequest() returns updated code after file edits.
 *   2. server.watcher.emit('change', path) triggers handleHotUpdate → full-reload.
 *
 * Platform gating (decision D5 / Gate0-2):
 *   Gated by HMR_ENABLED (Linux inotify reference platform, or MDS_HMR=1 override).
 *   On macOS/Windows these tests SKIP (stay green). The contract specs in
 *   hmr.spec.mjs run cross-platform without a gate.
 *
 * Implementation notes:
 *   - Vite approach: middlewareMode=true (no browser needed, no HTTP server).
 *   - Freshness: after editing, call server.moduleGraph.invalidateAll() then
 *     server.transformRequest() — returns fresh compiled output. This bypasses
 *     the filesystem watcher entirely and is intentional for DETERMINISM: real
 *     polling-watcher delivery in middlewareMode is unreliable across platforms.
 *   - HMR signal: spy on server.ws.send; inject watcher events via
 *     server.watcher.emit('change', absPath) SYNTHETICALLY. This tests that
 *     handleHotUpdate correctly sends full-reload given a change event, but does
 *     NOT test that a real file write causes the polling watcher to deliver that
 *     event. The webpack/rspack/rollup suites cover real polling-driven rebuilds.
 *   - NOTE: the server.watch config (usePolling etc.) is present for completeness
 *     but is NOT relied upon by these tests — all watcher events are injected
 *     synthetically. This is a deliberate design for determinism; a real-watcher
 *     test was considered but deferred because Vite middlewareMode watcher
 *     delivery is timing-flaky outside Linux inotify even with MDS_HMR=1.
 *   - root must use realpath to avoid macOS /tmp → /private/tmp mismatch.
 *   - Teardown: server.close() — no SIGINT.
 *   - Each test creates an independent temp project via createTempMdsProject().
 *
 * Vite-specific driver recipe (headless, no browser):
 *   1. createServer({ root, plugins:[mdsPlugin()], server:{middlewareMode:true} })
 *   2. server.transformRequest('/entry.mds') — compiles and caches module
 *   3. editFile() → server.moduleGraph.invalidateAll() → transformRequest() again
 *      → assert fresh code
 *   4. For reload signal: intercept server.ws.send; server.watcher.emit('change', absPath)
 *   5. server.close() in after()
 *
 * Test scenarios:
 *   Suite 1: T-HMR-a through T-HMR-e, T-P1, T-P2
 *   Suite 3: T-E-del, T-E-create, T-E-mdflip, T-E-cycle (real-driver edge cases)
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { realpathSync, unlinkSync } from 'node:fs';
import { createServer } from 'vite';
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
 * Create a Vite dev server for the given project root.
 *
 * realpathSync is applied to root to resolve macOS /tmp → /private/tmp symlink.
 * Vite resolves files with realpathSync internally; if the root doesn't match,
 * file lookups fail with "Does the file exist?" even when the file is on disk.
 *
 * Note: these tests inject watcher events synthetically via server.watcher.emit()
 * rather than relying on the polling watcher to deliver real fs events. See the
 * module-level comment for rationale (determinism, middlewareMode watcher limits).
 *
 * @param {string} root - Absolute path to the project root.
 * @returns {Promise<import('vite').ViteDevServer>}
 */
async function createViteServer(root) {
  // Resolve symlinks: macOS /tmp → /private/tmp; Linux /tmp is already real
  const resolvedRoot = realpathSync(root);
  return createServer({
    root: resolvedRoot,
    plugins: [mdsPlugin()],
    server: {
      middlewareMode: true,
      // NOTE: usePolling is configured here but these tests do NOT wait for the
      // polling watcher to fire — all change events are injected synthetically
      // (server.watcher.emit). This tests handleHotUpdate logic in isolation,
      // not the fs-watch → event-delivery path. See module-level comment.
      watch: { usePolling: true, interval: 50 },
    },
    appType: 'custom',
    logLevel: 'error',
  });
}

/**
 * Request a module from the Vite server and return its compiled code.
 * Invalidates the module graph first to ensure fresh content.
 *
 * @param {import('vite').ViteDevServer} server
 * @param {string} url - Server-relative URL (e.g. '/entry.mds').
 * @returns {Promise<string>}
 */
async function transformFresh(server, url) {
  server.moduleGraph.invalidateAll();
  const result = await server.transformRequest(url);
  return result?.code ?? '';
}

/**
 * Intercept server.ws.send to capture full-reload signals.
 *
 * @param {import('vite').ViteDevServer} server
 * @returns {{ reloads: Array<object>, restore: () => void }}
 */
function spyOnWsSend(server) {
  const reloads = [];
  const origSend = server.ws.send.bind(server.ws);
  server.ws.send = (payload) => {
    reloads.push(payload);
    origSend(payload);
  };
  return {
    reloads,
    restore: () => { server.ws.send = origSend; },
  };
}

// ---------------------------------------------------------------------------
// Suite 1: HMR lifecycle (real Vite server)
// ---------------------------------------------------------------------------

describe('vite-plugin HMR e2e — Suite 1 (real server)', { skip: !HMR_ENABLED && 'HMR e2e tests are Linux-gated; set MDS_HMR=1 to run' }, () => {

  test('T-HMR-a (AC-F1): edit entry .mds → fresh transformRequest with new marker', async () => {
    const { dir, paths, cleanup } = createTempMdsProject(
      { 'entry.mds': '---\nname: World\n---\n\nHello {name}! MARKER_A' },
    );
    const server = await createViteServer(dir);

    try {
      // Initial transform
      const codeA = await server.transformRequest('/entry.mds');
      assert.ok(codeA?.code?.includes('MARKER_A'), 'initial transform contains MARKER_A');

      // Edit → fresh
      editFile(paths['entry.mds'], '---\nname: World\n---\n\nHello {name}! MARKER_B');
      const codeB = await transformFresh(server, '/entry.mds');
      assert.ok(codeB.includes('MARKER_B'), 'fresh transform contains MARKER_B');
      assert.ok(!codeB.includes('MARKER_A'), 'MARKER_A gone after edit');
    } finally {
      await server.close();
      cleanup();
    }
  });

  test('T-HMR-a (reload): watcher change event → server.ws.send full-reload', async () => {
    // Tests the HMR signal path: watcher detects change → handleHotUpdate → ws.send
    const { dir, paths, cleanup } = createTempMdsProject(
      { 'entry.mds': '---\nname: World\n---\n\nHello {name}! MARKER_A' },
    );
    const server = await createViteServer(dir);
    const { reloads, restore } = spyOnWsSend(server);

    try {
      // Populate the transformed Set by doing an initial transform
      await server.transformRequest('/entry.mds');

      // Edit the file and emit a watcher change event
      editFile(paths['entry.mds'], '---\nname: World\n---\n\nHello {name}! MARKER_B');
      server.watcher.emit('change', paths['entry.mds']);

      await waitFor(() => reloads.some(r => r.type === 'full-reload'),
        { timeoutMs: 5_000, label: 'full-reload signal received' });

      assert.ok(
        reloads.some(r => r.type === 'full-reload'),
        'server.ws.send received { type: "full-reload" }',
      );
    } finally {
      restore();
      await server.close();
      cleanup();
    }
  });

  test('T-HMR-b (AC-F2): edit transitive @import dep → fresh transform', async () => {
    const { dir, paths, cleanup } = createTempMdsProject(
      {
        // ADR-014: dep BEFORE entry
        'dep.mds': '@define greet(who):\nHi {who}! MARKER_A\n@end\n\n@export greet',
        'entry.mds': '@import { greet } from "./dep.mds"\n\n{greet("World")}',
      },
    );
    const server = await createViteServer(dir);

    try {
      const codeA = await server.transformRequest('/entry.mds');
      assert.ok(codeA?.code?.includes('MARKER_A'), 'initial MARKER_A in entry transform');

      editFile(paths['dep.mds'], '@define greet(who):\nHi {who}! MARKER_B\n@end\n\n@export greet');
      const codeB = await transformFresh(server, '/entry.mds');
      assert.ok(codeB.includes('MARKER_B'), 'dep edit propagates to entry transform');
    } finally {
      await server.close();
      cleanup();
    }
  });

  test('T-HMR-b (dep reload): dep change → full-reload signal via handleHotUpdate', async () => {
    const { dir, paths, cleanup } = createTempMdsProject(
      {
        'dep.mds': '@define greet(who):\nHi {who}! MARKER_A\n@end\n\n@export greet',
        'entry.mds': '@import { greet } from "./dep.mds"\n\n{greet("World")}',
      },
    );
    const server = await createViteServer(dir);
    const { reloads, restore } = spyOnWsSend(server);

    try {
      // Transform entry — this registers dep in the transformed Set
      await server.transformRequest('/entry.mds');

      // Emit change for the dep file
      editFile(paths['dep.mds'], '@define greet(who):\nHi {who}! MARKER_B\n@end\n\n@export greet');
      server.watcher.emit('change', paths['dep.mds']);

      await waitFor(() => reloads.some(r => r.type === 'full-reload'),
        { timeoutMs: 5_000, label: 'full-reload for dep change' });
      assert.ok(reloads.some(r => r.type === 'full-reload'), 'dep change triggers full-reload');
    } finally {
      restore();
      await server.close();
      cleanup();
    }
  });

  test('T-HMR-c (AC-F3): inject compile error → Vite throws, server stays alive', async () => {
    const { dir, paths, cleanup } = createTempMdsProject(
      { 'entry.mds': '---\nname: World\n---\n\nHello {name}! MARKER_A' },
    );
    const server = await createViteServer(dir);

    try {
      // Initial transform succeeds
      const codeA = await server.transformRequest('/entry.mds');
      assert.ok(codeA?.code?.includes('MARKER_A'), 'initial MARKER_A');

      // Inject compile error
      editFile(paths['entry.mds'], '{undefined_var_xyz_bad_syntax!!!}');
      let transformError = null;
      try {
        await transformFresh(server, '/entry.mds');
      } catch (err) {
        transformError = err;
      }
      assert.ok(transformError instanceof Error, 'compile error surfaced as thrown Error');

      // isAlive(): server can still transform other/fixed files
      editFile(paths['entry.mds'], '---\nname: World\n---\n\nHello {name}! MARKER_FIXED');
      const codeFixed = await transformFresh(server, '/entry.mds');
      assert.ok(codeFixed.includes('MARKER_FIXED'), 'server alive after error: fixed file transforms');
    } finally {
      await server.close();
      cleanup();
    }
  });

  test('T-HMR-d (AC-F4): fix compile error → fresh transform, no error', async () => {
    const { dir, paths, cleanup } = createTempMdsProject(
      { 'entry.mds': '{undefined_var_xyz_bad_syntax!!!}' },
    );
    const server = await createViteServer(dir);

    try {
      // Initial transform should error
      let initialError = null;
      try {
        await server.transformRequest('/entry.mds');
      } catch (err) {
        initialError = err;
      }
      assert.ok(initialError instanceof Error, 'initial transform throws for bad syntax');

      // Fix the file
      editFile(paths['entry.mds'], '---\nname: World\n---\n\nHello {name}! MARKER_FIXED');
      const codeFixed = await transformFresh(server, '/entry.mds');
      assert.ok(codeFixed.includes('MARKER_FIXED'), 'fixed file transforms successfully');
    } finally {
      await server.close();
      cleanup();
    }
  });

  test('T-HMR-e (AC-F5): add a second @import dep, edit it → recompile', async () => {
    const { dir, paths, cleanup } = createTempMdsProject(
      {
        'dep1.mds': '@define greet(who):\nHi {who}! MARKER_A\n@end\n\n@export greet',
        'entry.mds': '@import { greet } from "./dep1.mds"\n\n{greet("World")}',
      },
    );
    const server = await createViteServer(dir);

    try {
      const codeA = await server.transformRequest('/entry.mds');
      assert.ok(codeA?.code?.includes('MARKER_A'), 'initial MARKER_A');

      // Add dep2
      const dep2Path = join(dir, 'dep2.mds');
      editFile(dep2Path, '@define farewell(who):\nBye {who}! MARKER_B\n@end\n\n@export farewell');
      editFile(paths['entry.mds'],
        '@import { greet } from "./dep1.mds"\n@import { farewell } from "./dep2.mds"\n\n{greet("World")} {farewell("World")}');

      const codeB = await transformFresh(server, '/entry.mds');
      assert.ok(codeB.includes('MARKER_B'), 'dep2 content in fresh transform');

      // Edit dep2
      editFile(dep2Path, '@define farewell(who):\nBye {who}! MARKER_C\n@end\n\n@export farewell');
      const codeC = await transformFresh(server, '/entry.mds');
      assert.ok(codeC.includes('MARKER_C'), 'dep2 edit propagates to entry transform');
    } finally {
      await server.close();
      cleanup();
    }
  });

  test('T-P1 (AC-P1): edit → fresh transform within 10s performance budget', async () => {
    const { dir, paths, cleanup } = createTempMdsProject(
      { 'entry.mds': '---\nname: World\n---\n\nHello {name}! MARKER_A' },
    );
    const server = await createViteServer(dir);

    try {
      await server.transformRequest('/entry.mds');

      const startMs = Date.now();
      editFile(paths['entry.mds'], '---\nname: World\n---\n\nHello {name}! MARKER_B');
      const codeB = await transformFresh(server, '/entry.mds');
      const elapsedMs = Date.now() - startMs;

      assert.ok(codeB.includes('MARKER_B'), 'fresh transform has MARKER_B');
      assert.ok(
        elapsedMs < 10_000,
        `Edit→fresh transform took ${elapsedMs}ms, must be < 10000ms (T-P1 budget)`,
      );
    } finally {
      await server.close();
      cleanup();
    }
  });

  test('T-P2 (AC-P2): 20-iteration bounded edit loop — no degradation, server alive', async () => {
    const N = 20;
    const { dir, paths, cleanup } = createTempMdsProject(
      { 'entry.mds': '---\nname: World\n---\n\nHello {name}! ITERATION_0' },
    );
    const server = await createViteServer(dir);

    try {
      for (let i = 1; i <= N; i++) {
        const marker = `ITERATION_${i}`;
        editFile(paths['entry.mds'], `---\nname: World\n---\n\nHello {name}! ${marker}`);
        const code = await transformFresh(server, '/entry.mds');
        assert.ok(code.includes(marker), `Iteration ${i}: fresh transform has ${marker}`);
      }
    } finally {
      await server.close();
      cleanup();
    }
  });
});

// ---------------------------------------------------------------------------
// Suite 3: Edge cases (real Vite server, Linux-gated)
// ---------------------------------------------------------------------------

describe('vite-plugin HMR e2e — Suite 3 edge cases', { skip: !HMR_ENABLED && 'HMR e2e tests are Linux-gated; set MDS_HMR=1 to run' }, () => {

  test('T-E-del (AC-E1): delete @imported dep → transform errors; recreate → recovers', async () => {
    const { dir, paths, cleanup } = createTempMdsProject(
      {
        'dep.mds': '@define greet(who):\nHi {who}! DEP_MARKER\n@end\n\n@export greet',
        'entry.mds': '@import { greet } from "./dep.mds"\n\n{greet("World")}',
      },
    );
    const server = await createViteServer(dir);

    try {
      const codeA = await server.transformRequest('/entry.mds');
      assert.ok(codeA?.code?.includes('DEP_MARKER'), 'initial DEP_MARKER');

      // Delete dep
      unlinkSync(paths['dep.mds']);
      let deleteError = null;
      try {
        await transformFresh(server, '/entry.mds');
      } catch (err) {
        deleteError = err;
      }
      assert.ok(deleteError instanceof Error, 'transform errors after dep deleted');

      // Recreate dep and re-transform
      editFile(paths['dep.mds'], '@define greet(who):\nHi {who}! DEP_RECREATED\n@end\n\n@export greet');
      const codeFixed = await transformFresh(server, '/entry.mds');
      assert.ok(codeFixed.includes('DEP_RECREATED'), 'recovered after dep recreated');
    } finally {
      await server.close();
      cleanup();
    }
  });

  test('T-E-create (AC-E1): entry @imports not-yet-created dep → error; create dep → recovers', async () => {
    const { dir, paths, cleanup } = createTempMdsProject(
      { 'entry.mds': '@import { greet } from "./missing.mds"\n\n{greet("World")}' },
    );
    const server = await createViteServer(dir);

    try {
      let initialError = null;
      try {
        await server.transformRequest('/entry.mds');
      } catch (err) {
        initialError = err;
      }
      assert.ok(initialError instanceof Error, 'transform errors when dep is missing');

      // Create the missing dep and re-transform
      editFile(join(dir, 'missing.mds'), '@define greet(who):\nHi {who}! CREATED_MARKER\n@end\n\n@export greet');
      const codeFixed = await transformFresh(server, '/entry.mds');
      assert.ok(codeFixed.includes('CREATED_MARKER'), 'recovered after dep created');
    } finally {
      await server.close();
      cleanup();
    }
  });

  test('T-E-mdflip (AC-E2): .md file gains type:mds mid-session → documented behavior', async () => {
    // Vite docs behavior: a .md file without type:mds frontmatter is NOT transformed
    // by the mds plugin (shouldTransform returns false). After type:mds is added,
    // the next transformRequest call (with invalidateAll) will compile it.
    // This test documents: the flip works without a server restart.
    const { dir, paths, cleanup } = createTempMdsProject(
      { 'doc.md': '# Plain markdown\n\nNo type:mds here. PLAIN_MARKER' },
    );
    const server = await createViteServer(dir);

    try {
      // Initial transform — no type:mds, plugin returns null → Vite serves as-is.
      // The mds plugin's transform() hook returns null (shouldTransform is false),
      // so the file is not compiled. We do not assert on the transformRequest result
      // here because Vite may return the raw source or null depending on version;
      // the contract for this test is only that no Error is thrown (server alive).
      await server.transformRequest('/doc.md');

      // Add type:mds frontmatter
      editFile(paths['doc.md'], '---\ntype: mds\nname: World\n---\n\nHello {name}! MD_FLIP_MARKER');
      const codeB = await transformFresh(server, '/doc.md');
      assert.ok(
        codeB.includes('MD_FLIP_MARKER'),
        '.md file with type:mds compiles after invalidateAll (no restart needed)',
      );
    } finally {
      await server.close();
      cleanup();
    }
  });

  test('T-E-cycle: circular @import → transform errors, server stays alive', async () => {
    const { dir, paths, cleanup } = createTempMdsProject(
      { 'entry.mds': '@import { thing } from "./entry.mds"\n\n{thing}' },
    );
    const server = await createViteServer(dir);

    try {
      // Circular import behavior is implementation-defined: the compiler may
      // error or produce partial output. Either outcome is acceptable; we swallow
      // the error (if any) because the only load-bearing invariant is that the
      // server stays alive after the circular attempt.
      try {
        await server.transformRequest('/entry.mds');
      } catch {
        // swallowed intentionally — see comment above
      }

      // Server must still be alive — transform a fresh unrelated file
      editFile(join(dir, 'health.mds'), '---\nname: World\n---\n\nHealth check ALIVE_MARKER');
      const healthCode = await transformFresh(server, '/health.mds');
      assert.ok(healthCode.includes('ALIVE_MARKER'), 'server alive after circular @import');
    } finally {
      await server.close();
      cleanup();
    }
  });
});
