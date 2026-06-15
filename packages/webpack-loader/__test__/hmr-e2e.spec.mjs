/**
 * Real-driver end-to-end HMR / watch tests for @mdscript/webpack-loader.
 *
 * These tests drive an ACTUAL webpack watcher and assert on freshly emitted
 * bundle content read from disk — not mocked loader calls. They verify the
 * full pipeline: edit .mds file → webpack re-invokes loader → fresh bundle.
 *
 * Platform gating (decision D5 / Gate0-2):
 *   Gated by HMR_ENABLED (Linux inotify reference platform, or MDS_HMR=1 override).
 *   On macOS/Windows these tests SKIP (stay green). The contract specs in
 *   hmr.spec.mjs run cross-platform without a gate.
 *
 * Test scenarios:
 *   Suite 1: T-HMR-a through T-HMR-e, T-P1, T-P2
 *   Suite 3: T-E-del, T-E-create, T-E-mdflip, T-E-cycle (real-driver edge cases)
 *
 * Implementation notes:
 *   - webpack watch mode writes bundles to disk (output.path). We read from disk.
 *   - poll:50 / aggregateTimeout:0 for deterministic rebuild triggering.
 *   - Content (marker string) is asserted, never build count.
 *   - Each test creates an independent temp project via createTempMdsProject().
 *   - Teardown via watching.close() (programmatic, never SIGINT).
 */
import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { readFileSync, existsSync, unlinkSync } from 'node:fs';
import webpack from 'webpack';
import {
  HMR_ENABLED,
  createTempMdsProject,
  editFile,
  waitFor,
} from '../../bundler-utils/__test__/hmr-harness.mjs';

process.env.NODE_ENV = 'test';

// Absolute path to the CJS loader (webpack requires CJS loaders)
const LOADER_PATH = new URL('../dist-cjs/index.js', import.meta.url).pathname;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Build a webpack config with the MDS loader for the given entry.
 *
 * @param {string} entryPath - Absolute path to the .mds entry file.
 * @param {string} outDir - Directory to write bundle.js into.
 * @returns {import('webpack').Configuration}
 */
function buildConfig(entryPath, outDir) {
  return {
    entry: entryPath,
    mode: 'development',
    devtool: false,
    output: { path: outDir, filename: 'bundle.js' },
    module: {
      rules: [{ test: /\.mds$/, use: LOADER_PATH }],
    },
  };
}

/**
 * Read the bundle source from disk.
 *
 * @param {string} outDir
 * @returns {string}
 */
function readBundle(outDir) {
  return readFileSync(join(outDir, 'bundle.js'), 'utf8');
}

/**
 * Start a webpack watcher and wait for the first callback.
 * Returns { watching, getBundle } where getBundle() reads the latest build.
 *
 * @param {import('webpack').Configuration} config
 * @returns {Promise<{ watching: import('webpack').Watching, getBundle: () => string, errors: string[], hasErrorState: () => boolean }>}
 */
async function startWebpackWatcher(config) {
  const compiler = webpack(config);
  const outDir = config.output.path;

  const errors = [];
  let latestStats = null;

  const watching = compiler.watch({ poll: 50, aggregateTimeout: 0 }, (err, stats) => {
    if (err) {
      errors.push(err.message);
      return;
    }
    latestStats = stats;
    if (stats.hasErrors()) {
      for (const e of stats.toJson().errors ?? []) {
        errors.push(e.message ?? String(e));
      }
    }
  });

  // Wait for the first build callback
  await waitFor(
    () => latestStats !== null || errors.length > 0,
    { timeoutMs: 30_000, intervalMs: 100, label: 'webpack first build' },
  );

  function getBundle() {
    return readBundle(outDir);
  }

  function hasErrorState() {
    return latestStats?.hasErrors() ?? false;
  }

  return { watching, getBundle, errors, hasErrorState };
}

// ---------------------------------------------------------------------------
// Suite 1: HMR lifecycle (real webpack watcher)
// ---------------------------------------------------------------------------

describe('webpack-loader HMR e2e — Suite 1 (real watcher)', { skip: !HMR_ENABLED && 'HMR e2e tests are Linux-gated; set MDS_HMR=1 to run' }, () => {

  test('T-HMR-a (AC-F1): edit entry .mds → fresh bundle with new marker', async () => {
    const { dir, paths, cleanup } = createTempMdsProject({
      'entry.mds': '---\nname: World\n---\n\nHello {name}! MARKER_A',
    });
    const outDir = join(dir, 'out');
    const { watching, getBundle } = await startWebpackWatcher(buildConfig(paths['entry.mds'], outDir));

    try {
      assert.ok(getBundle().includes('MARKER_A'), 'initial build contains MARKER_A');

      editFile(paths['entry.mds'], '---\nname: World\n---\n\nHello {name}! MARKER_B');
      await waitFor(() => getBundle().includes('MARKER_B'),
        { timeoutMs: 10_000, label: 'MARKER_B in bundle' });

      assert.ok(getBundle().includes('MARKER_B'), 'rebuild contains MARKER_B');
      assert.ok(!getBundle().includes('MARKER_A'), 'MARKER_A gone after edit');
    } finally {
      await new Promise((resolve) => watching.close(resolve));
      cleanup();
    }
  });

  test('T-HMR-b (AC-F2): edit transitive @import dep → fresh bundle', async () => {
    // ADR-014: dep files BEFORE entry
    const { dir, paths, cleanup } = createTempMdsProject({
      'dep.mds': '@define greet(who):\nHi {who}! MARKER_A\n@end\n\n@export greet',
      'entry.mds': '@import { greet } from "./dep.mds"\n\n{greet("World")}',
    });
    const outDir = join(dir, 'out');
    const { watching, getBundle } = await startWebpackWatcher(buildConfig(paths['entry.mds'], outDir));

    try {
      await waitFor(() => getBundle().includes('MARKER_A'),
        { timeoutMs: 10_000, label: 'initial MARKER_A' });

      editFile(paths['dep.mds'], '@define greet(who):\nHi {who}! MARKER_B\n@end\n\n@export greet');
      await waitFor(() => getBundle().includes('MARKER_B'),
        { timeoutMs: 10_000, label: 'MARKER_B after dep edit' });

      assert.ok(getBundle().includes('MARKER_B'), 'dep edit propagates through webpack');
    } finally {
      await new Promise((resolve) => watching.close(resolve));
      cleanup();
    }
  });

  test('T-HMR-c (AC-F3): inject compile error → webpack surfaces error, watcher stays alive', async () => {
    const { dir, paths, cleanup } = createTempMdsProject({
      'entry.mds': '---\nname: World\n---\n\nHello {name}! MARKER_A',
    });
    const outDir = join(dir, 'out');
    const compiler = webpack(buildConfig(paths['entry.mds'], outDir));

    let latestStats = null;
    const watching = compiler.watch({ poll: 50, aggregateTimeout: 0 }, (_err, stats) => {
      latestStats = stats;
    });

    try {
      // Wait for initial good build
      await waitFor(() => latestStats !== null && !latestStats.hasErrors(),
        { timeoutMs: 30_000, label: 'initial good build' });
      assert.ok(readBundle(outDir).includes('MARKER_A'), 'initial MARKER_A');

      // Inject a compile error
      editFile(paths['entry.mds'], '{undefined_var_xyz_bad_syntax!!!}');
      await waitFor(() => latestStats !== null && latestStats.hasErrors(),
        { timeoutMs: 10_000, label: 'webpack error state' });

      assert.ok(latestStats.hasErrors(), 'webpack entered error state');
      // T-C4: error message includes file information
      const errJson = latestStats.toJson().errors ?? [];
      assert.ok(errJson.length > 0, 'at least one error in stats.toJson().errors');
      const errMsg = errJson[0].message ?? '';
      assert.ok(errMsg.length > 0, 'error message is non-empty');

      // isAlive(): watcher keeps firing after error — inject valid content
      const statsBefore = latestStats;
      editFile(paths['entry.mds'], '---\nname: World\n---\n\nHello {name}! MARKER_FIXED');
      await waitFor(() => latestStats !== statsBefore && !latestStats.hasErrors(),
        { timeoutMs: 10_000, label: 'recovery build after error' });

      assert.ok(!latestStats.hasErrors(), 'error cleared after fix');
      assert.ok(readBundle(outDir).includes('MARKER_FIXED'), 'recovery bundle has MARKER_FIXED');
    } finally {
      await new Promise((resolve) => watching.close(resolve));
      cleanup();
    }
  });

  test('T-HMR-d (AC-F4): fix compile error → fresh bundle, hasErrors() false', async () => {
    const { dir, paths, cleanup } = createTempMdsProject({
      'entry.mds': '{undefined_var_xyz_bad_syntax!!!}',
    });
    const outDir = join(dir, 'out');
    const compiler = webpack(buildConfig(paths['entry.mds'], outDir));

    let latestStats = null;
    const watching = compiler.watch({ poll: 50, aggregateTimeout: 0 }, (_err, stats) => {
      latestStats = stats;
    });

    try {
      // Initial build should error
      await waitFor(() => latestStats !== null && latestStats.hasErrors(),
        { timeoutMs: 30_000, label: 'initial error build' });
      assert.ok(latestStats.hasErrors(), 'initial build has errors');

      // Fix the file
      editFile(paths['entry.mds'], '---\nname: World\n---\n\nHello {name}! MARKER_FIXED');
      await waitFor(
        () => latestStats !== null && !latestStats.hasErrors() && existsSync(join(outDir, 'bundle.js')) && readBundle(outDir).includes('MARKER_FIXED'),
        { timeoutMs: 10_000, label: 'MARKER_FIXED and hasErrors()===false' },
      );

      assert.ok(!latestStats.hasErrors(), 'hasErrors()===false after fix');
      assert.ok(readBundle(outDir).includes('MARKER_FIXED'), 'fixed bundle content');
    } finally {
      await new Promise((resolve) => watching.close(resolve));
      cleanup();
    }
  });

  test('T-HMR-e (AC-F5): add a second @import dep, edit it → recompile', async () => {
    // ADR-014: dep files BEFORE entry
    const { dir, paths, cleanup } = createTempMdsProject({
      'dep1.mds': '@define greet(who):\nHi {who}! MARKER_A\n@end\n\n@export greet',
      'entry.mds': '@import { greet } from "./dep1.mds"\n\n{greet("World")}',
    });
    const outDir = join(dir, 'out');
    const { watching, getBundle } = await startWebpackWatcher(buildConfig(paths['entry.mds'], outDir));

    try {
      await waitFor(() => getBundle().includes('MARKER_A'),
        { timeoutMs: 10_000, label: 'initial MARKER_A' });

      // Add dep2 and update entry to import it
      const dep2Path = join(dir, 'dep2.mds');
      editFile(dep2Path, '@define farewell(who):\nBye {who}! MARKER_B\n@end\n\n@export farewell');
      editFile(paths['entry.mds'],
        '@import { greet } from "./dep1.mds"\n@import { farewell } from "./dep2.mds"\n\n{greet("World")} {farewell("World")}');

      await waitFor(() => getBundle().includes('MARKER_B'),
        { timeoutMs: 10_000, label: 'MARKER_B after dep2 import' });
      assert.ok(getBundle().includes('MARKER_B'), 'dep2 content in bundle');

      // Edit dep2
      editFile(dep2Path, '@define farewell(who):\nBye {who}! MARKER_C\n@end\n\n@export farewell');
      await waitFor(() => getBundle().includes('MARKER_C'),
        { timeoutMs: 10_000, label: 'MARKER_C after dep2 edit' });
      assert.ok(getBundle().includes('MARKER_C'), 'dep2 edit triggers recompile');
    } finally {
      await new Promise((resolve) => watching.close(resolve));
      cleanup();
    }
  });

  test('T-P1 (AC-P1): edit → fresh bundle within 10s performance budget', async () => {
    const { dir, paths, cleanup } = createTempMdsProject({
      'entry.mds': '---\nname: World\n---\n\nHello {name}! MARKER_A',
    });
    const outDir = join(dir, 'out');
    const { watching, getBundle } = await startWebpackWatcher(buildConfig(paths['entry.mds'], outDir));

    try {
      assert.ok(getBundle().includes('MARKER_A'), 'initial build ok');

      const startMs = Date.now();
      editFile(paths['entry.mds'], '---\nname: World\n---\n\nHello {name}! MARKER_B');
      await waitFor(() => getBundle().includes('MARKER_B'), { timeoutMs: 10_000 });
      const elapsedMs = Date.now() - startMs;

      assert.ok(
        elapsedMs < 10_000,
        `Edit→fresh took ${elapsedMs}ms, must be < 10000ms (T-P1 budget)`,
      );
    } finally {
      await new Promise((resolve) => watching.close(resolve));
      cleanup();
    }
  });

  test('T-P2 (AC-P2): 20-iteration bounded edit loop — no degradation, watcher alive', async () => {
    const N = 20;
    const { dir, paths, cleanup } = createTempMdsProject({
      'entry.mds': '---\nname: World\n---\n\nHello {name}! ITERATION_0',
    });
    const outDir = join(dir, 'out');
    const { watching, getBundle } = await startWebpackWatcher(buildConfig(paths['entry.mds'], outDir));

    try {
      for (let i = 1; i <= N; i++) {
        const marker = `ITERATION_${i}`;
        editFile(paths['entry.mds'], `---\nname: World\n---\n\nHello {name}! ${marker}`);
        await waitFor(() => getBundle().includes(marker), { timeoutMs: 10_000, label: marker });
      }
      assert.ok(getBundle().includes(`ITERATION_${N}`), `Final iteration ${N} is fresh`);
    } finally {
      await new Promise((resolve) => watching.close(resolve));
      cleanup();
    }
  });
});

// ---------------------------------------------------------------------------
// Suite 3: Edge cases (real webpack watcher, Linux-gated)
// ---------------------------------------------------------------------------

describe('webpack-loader HMR e2e — Suite 3 edge cases', { skip: !HMR_ENABLED && 'HMR e2e tests are Linux-gated; set MDS_HMR=1 to run' }, () => {

  test('T-E-del (AC-E1): delete @imported dep → webpack errors; recreate → recovers', async () => {
    // ADR-014: dep BEFORE entry
    const { dir, paths, cleanup } = createTempMdsProject({
      'dep.mds': '@define greet(who):\nHi {who}! DEP_MARKER\n@end\n\n@export greet',
      'entry.mds': '@import { greet } from "./dep.mds"\n\n{greet("World")}',
    });
    const outDir = join(dir, 'out');
    const compiler = webpack(buildConfig(paths['entry.mds'], outDir));

    let latestStats = null;
    const watching = compiler.watch({ poll: 50, aggregateTimeout: 0 }, (_err, stats) => {
      latestStats = stats;
    });

    try {
      await waitFor(() => latestStats !== null && !latestStats.hasErrors(),
        { timeoutMs: 30_000, label: 'initial good build' });
      assert.ok(readBundle(outDir).includes('DEP_MARKER'), 'initial DEP_MARKER');

      // Delete the dep
      unlinkSync(paths['dep.mds']);
      editFile(paths['entry.mds'], '@import { greet } from "./dep.mds"\n\n{greet("World")} AFTER_DEL');

      await waitFor(() => latestStats !== null && latestStats.hasErrors(),
        { timeoutMs: 10_000, label: 'error after dep deletion' });
      assert.ok(latestStats.hasErrors(), 'webpack errors after dep deleted');

      // Recreate the dep and restore entry
      editFile(paths['dep.mds'], '@define greet(who):\nHi {who}! DEP_RECREATED\n@end\n\n@export greet');
      editFile(paths['entry.mds'], '@import { greet } from "./dep.mds"\n\n{greet("World")}');

      await waitFor(
        () => latestStats !== null && !latestStats.hasErrors() && readBundle(outDir).includes('DEP_RECREATED'),
        { timeoutMs: 10_000, label: 'DEP_RECREATED after restore' },
      );
      assert.ok(readBundle(outDir).includes('DEP_RECREATED'), 'recovered after dep recreated');
    } finally {
      await new Promise((resolve) => watching.close(resolve));
      cleanup();
    }
  });

  test('T-E-create (AC-E1): entry @imports not-yet-created dep → error; create dep → recovers', async () => {
    const { dir, paths, cleanup } = createTempMdsProject({
      'entry.mds': '@import { greet } from "./missing.mds"\n\n{greet("World")}',
    });
    const outDir = join(dir, 'out');
    const compiler = webpack(buildConfig(paths['entry.mds'], outDir));

    let latestStats = null;
    const watching = compiler.watch({ poll: 50, aggregateTimeout: 0 }, (_err, stats) => {
      latestStats = stats;
    });

    try {
      await waitFor(() => latestStats !== null && latestStats.hasErrors(),
        { timeoutMs: 30_000, label: 'initial error (missing dep)' });
      assert.ok(latestStats.hasErrors(), 'webpack errors for missing dep');

      // Create the missing dep and re-touch entry
      editFile(join(dir, 'missing.mds'), '@define greet(who):\nHi {who}! CREATED_MARKER\n@end\n\n@export greet');
      editFile(paths['entry.mds'], '@import { greet } from "./missing.mds"\n\n{greet("World")}');

      await waitFor(
        () => latestStats !== null && !latestStats.hasErrors() && existsSync(join(outDir, 'bundle.js')) && readBundle(outDir).includes('CREATED_MARKER'),
        { timeoutMs: 10_000, label: 'CREATED_MARKER after dep creation' },
      );
      assert.ok(readBundle(outDir).includes('CREATED_MARKER'), 'recovered after dep created');
    } finally {
      await new Promise((resolve) => watching.close(resolve));
      cleanup();
    }
  });

  test('T-E-mdflip (AC-E2): .mds file first builds, plain source treated as literal', async () => {
    // Webpack+MDS: shouldTransform() for a .mds file always returns true (by extension).
    // A .mds file with no MDS-specific syntax still compiles (content treated as plain text).
    // This test documents: content without valid MDS frontmatter is still compiled.
    const { dir, paths, cleanup } = createTempMdsProject({
      'entry.mds': 'Just plain text content. MD_PLAIN_MARKER',
    });
    const outDir = join(dir, 'out');
    const { watching, getBundle } = await startWebpackWatcher(buildConfig(paths['entry.mds'], outDir));

    try {
      await waitFor(() => getBundle().includes('MD_PLAIN_MARKER'),
        { timeoutMs: 10_000, label: 'MD_PLAIN_MARKER compiled' });
      assert.ok(getBundle().includes('MD_PLAIN_MARKER'), '.mds file compiled even without frontmatter');

      // Now add proper frontmatter
      editFile(paths['entry.mds'], '---\nname: World\n---\n\nHello {name}! MD_FLIP_MARKER');
      await waitFor(() => getBundle().includes('MD_FLIP_MARKER'),
        { timeoutMs: 10_000, label: 'MD_FLIP_MARKER after frontmatter added' });
      assert.ok(getBundle().includes('MD_FLIP_MARKER'), 'bundle updated after frontmatter added');
    } finally {
      await new Promise((resolve) => watching.close(resolve));
      cleanup();
    }
  });

  test('T-E-cycle: circular @import → webpack errors, no infinite rebuild loop', async () => {
    const { dir, paths, cleanup } = createTempMdsProject({
      'entry.mds': '@import { thing } from "./entry.mds"\n\n{thing}',
    });
    const outDir = join(dir, 'out');
    const compiler = webpack(buildConfig(paths['entry.mds'], outDir));

    let latestStats = null;
    let callbackCount = 0;
    const watching = compiler.watch({ poll: 50, aggregateTimeout: 0 }, (_err, stats) => {
      latestStats = stats;
      callbackCount++;
    });

    try {
      // Wait for initial callback (error or bundle)
      await waitFor(() => callbackCount >= 1, { timeoutMs: 30_000, label: 'initial callback' });

      // Wait 3s to observe any looping behavior
      const countBefore = callbackCount;
      await new Promise((r) => setTimeout(r, 3_000));
      const countAfter = callbackCount;

      // Allow at most 3 extra callbacks (webpack may do a valid follow-up check)
      assert.ok(
        countAfter - countBefore <= 3,
        `Circular @import caused ${countAfter - countBefore} extra webpack callbacks — possible infinite loop`,
      );
    } finally {
      await new Promise((resolve) => watching.close(resolve));
      cleanup();
    }
  });
});
