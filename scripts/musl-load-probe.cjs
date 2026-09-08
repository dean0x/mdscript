// musl-load-probe.cjs — Alpine container smoke-test for musl napi addon load.
// #340, PF-013: the fixture shape IS the assertion; a pass is only possible if
// the correct musl platform package is mounted under /w/node_modules/ AND the
// loader's isMusl() returned true. Run inside `node:22-alpine` via:
//   docker run --rm --network none --pull=never -w /w -v <staged-dir>:/w:ro <image> \
//     node /w/probe.cjs <platform>
//
// Steps: 1 argv, 2 cwd, 3 ldd/musl, 4 loader require, 5 path resolution,
//        6 exports, 7 compile smoke test.
//
// Fixture at /w: index.js (real loader), probe.cjs (this file),
//               node_modules/@mdscript/mds-napi-<platform>/ (musl pkg only).
'use strict';

const VALID_PLATFORMS = ['linux-x64-musl', 'linux-arm64-musl'];
const platform = process.argv[2];

// Step 1: Validate argv — missing, extra, or unknown platform → exit 2.
if (process.argv.length !== 3 || !VALID_PLATFORMS.includes(platform)) {
  process.stderr.write(
    '::error::Usage: node probe.cjs <platform>\n' +
    '  platform must be one of: ' + VALID_PLATFORMS.join(', ') + '\n' +
    '  got: ' + JSON.stringify(process.argv.slice(2)) + '\n',
  );
  process.exit(2);
}

// Step 2: Assert cwd is /w — node:22-alpine sets no WORKDIR so the default cwd is /;
// mds-core rejects a filesystem-root base directory (#371, found by this gate's first
// run); the docker run must pass -w /w so this probe runs from the fixture dir.
if (process.cwd() !== '/w') {
  process.stderr.write(
    '::error::probe must run with cwd /w (docker run -w /w); got ' +
    process.cwd() + ' — a root cwd trips the mds-core base-directory defect (#371)\n',
  );
  process.exit(1);
}

// Step 3: Verify musl via /usr/bin/ldd — re-implements isMusl() from index.js
// verbatim (readFileSync('/usr/bin/ldd','utf-8').includes('musl') inside try/catch).
// This check stays even though require() below also proves it — it makes the
// isMusl() predicate visible in the log (PF-013: absence-only check is vacuous).
const { readFileSync } = require('fs');
let lddContent;
try {
  lddContent = readFileSync('/usr/bin/ldd', 'utf-8');
} catch (e) {
  process.stderr.write('::error::Cannot read /usr/bin/ldd: ' + ((e && e.message) || String(e)) + '\n');
  process.exit(1);
}
if (!lddContent.includes('musl')) {
  process.stderr.write(
    '::error::isMusl() predicate failed: /usr/bin/ldd (' +
    lddContent.length + ' chars) does not include "musl". ' +
    'This probe must run inside node:22-alpine, not a glibc container.\n',
  );
  process.exit(1);
}
const lddBytes = Buffer.byteLength(lddContent, 'utf-8');
process.stdout.write(
  'musl detected via /usr/bin/ldd (' + lddBytes + ' bytes), ' +
  'platform=' + process.platform + ' arch=' + process.arch + '\n',
);

// Step 4: Load the real loader — never require the .node directly and never
// @mdscript/mds (its WASM fallback would make the test vacuous).
// Wrapped in try/catch so a load failure prints the full loader error message
// (including per-candidate details) via ::error:: before exiting, giving the
// three workflow needles their signal.
let b;
try {
  b = require('/w/index.js');
} catch (e) {
  process.stderr.write('::error::require(\'/w/index.js\') failed: ' + ((e && e.message) || String(e)) + '\n');
  process.exit(1);
}

// Step 5: Verify require.resolve path for the musl platform package.
// Must start with /w/node_modules/ and end with mds-napi.<platform>.node,
// proving the loader used the fixture package, not a stale path or fallback.
const pkg = '@mdscript/mds-napi-' + platform;
let resolved;
try {
  resolved = require.resolve(pkg);
} catch (e) {
  process.stderr.write('::error::require.resolve(\'' + pkg + '\') failed: ' + ((e && e.message) || String(e)) + '\n');
  process.exit(1);
}
if (!resolved.startsWith('/w/node_modules/')) {
  process.stderr.write(
    '::error::resolved path for \'' + pkg + '\' must start with /w/node_modules/; ' +
    'got: ' + resolved + '\n',
  );
  process.exit(1);
}
const expectedSuffix = 'mds-napi.' + platform + '.node';
if (!resolved.endsWith(expectedSuffix)) {
  process.stderr.write(
    '::error::resolved path for \'' + pkg + '\' must end with \'' + expectedSuffix + '\'; ' +
    'got: ' + resolved + '\n',
  );
  process.exit(1);
}
process.stdout.write('resolved ' + pkg + ' -> ' + resolved + '\n');

// Step 6: Verify exports are exactly the 7 required keys.
const EXPECTED_EXPORTS = 'check,checkFile,compile,compileFile,lint,lintFile,lintVirtual';
const actualExports = Object.keys(b).sort().join(',');
if (actualExports !== EXPECTED_EXPORTS) {
  process.stderr.write(
    '::error::addon exports mismatch.\n' +
    '  expected: ' + EXPECTED_EXPORTS + '\n' +
    '  actual:   ' + actualExports + '\n',
  );
  process.exit(1);
}

// Step 7: Compile smoke test. dlopen binds lazily so symbols resolve at CALL
// time — one export's type is not proof; we must call an exported function.
// Expected: { kind: 'markdown', output: 'Hello alpine!\n', ... }
let r;
try {
  r = b.compile('Hello {{n}}!', { vars: { n: 'alpine' } });
} catch (e) {
  process.stderr.write('::error::compile() threw: ' + ((e && e.message) || String(e)) + '\n');
  process.exit(1);
}
if (r.kind !== 'markdown') {
  process.stderr.write(
    '::error::compile() returned kind=' + JSON.stringify(r.kind) + ', expected "markdown"\n',
  );
  process.exit(1);
}
const EXPECTED_OUTPUT = 'Hello alpine!\n';
if (r.output !== EXPECTED_OUTPUT) {
  process.stderr.write(
    '::error::compile() output mismatch.\n' +
    '  expected: ' + JSON.stringify(EXPECTED_OUTPUT) + '\n' +
    '  actual:   ' + JSON.stringify(r.output) + '\n',
  );
  process.exit(1);
}
process.stdout.write('load ok: compile(...) -> ' + JSON.stringify(r.output) + '\n');
