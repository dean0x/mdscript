import { execFileSync } from 'node:child_process';
import { readFileSync, realpathSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const PIN_FILE = '.github/actions/setup-wasm/action.yml';
const VERSION_TIMEOUT_MS = 3000;
const VERSION_MAX_BYTES = 16 * 1024;
/** @type {readonly ['wasm-pack', 'wasm-opt']} */
const TOOLS = ['wasm-pack', 'wasm-opt'];

/** @typedef {(typeof TOOLS)[number]} Tool */
/** @template T @typedef {{ok: true, value: T} | {ok: false, error: string}} Result */
/** @typedef {{exitCode: number, stdout?: string, stderr?: string}} Decision */

/**
 * Read the two plain version values in the composite action, in tool order.
 * @param {string} text
 * @returns {Result<Record<Tool, string>>}
 */
export function readToolPins(text) {
  const blocks = text.match(/^[ \t]+with[ \t]*:/gm) ?? [];
  const versions = text.match(/^[ \t]+version[ \t]*:/gm) ?? [];
  const pins = [...text.matchAll(/^      with:[ \t]*(?:[ \t]+#[^\r\n]*)?\r?\n        version:[ \t]+([^\s#]+)(?:[ \t]+#[^\r\n]*)?[ \t]*\r?$/gm)];
  if (blocks.length !== 2 || versions.length !== 2 || pins.length !== 2) {
    return { ok: false, error: 'expected exactly two tool pins in with blocks' };
  }
  const pack = /^v?(\d+\.\d+\.\d+)$/.exec(pins[0][1]);
  if (!pack) return { ok: false, error: 'invalid wasm-pack pin' };
  const opt = pins[1][1];
  if (!/^\d+$/.test(opt)) return { ok: false, error: 'invalid wasm-opt pin' };
  return { ok: true, value: { 'wasm-pack': pack[1], 'wasm-opt': opt } };
}

/**
 * @param {Tool} tool
 * @param {string} output
 * @returns {Result<string>}
 */
export function parseToolVersion(tool, output) {
  const pattern = tool === 'wasm-pack'
    ? /^wasm-pack v?(\d+\.\d+\.\d+)$/
    : /^wasm-opt version (\d+)(?: \(version_\1\))?$/;
  const match = pattern.exec(output.trim());
  if (!match) return { ok: false, error: `cannot parse ${tool} --version output` };
  return { ok: true, value: match[1] };
}

/** @param {boolean} strict @param {string[]} problems @returns {Decision} */
function failure(strict, problems) {
  const label = strict ? 'WASM tool check failed' : 'WASM tool drift warning';
  return { exitCode: strict ? 1 : 0, stderr: `${label}: ${problems.join('; ')} (pins: ${PIN_FILE})` };
}

/**
 * @param {boolean} strict
 * @param {Record<Tool, string>} pins
 * @param {Record<Tool, Result<string>>} installed
 * @returns {Decision}
 */
export function decideToolCheck(strict, pins, installed) {
  const problems = [];
  for (const tool of TOOLS) {
    const version = installed[tool];
    if (!version.ok) problems.push(version.error);
    else if (version.value !== pins[tool]) {
      problems.push(`${tool} ${version.value} does not match pin ${pins[tool]}`);
    }
  }
  if (problems.length > 0) return failure(strict, problems);
  return { exitCode: 0, stdout: strict ? `WASM tools match pins: wasm-pack ${pins['wasm-pack']}; wasm-opt ${pins['wasm-opt']}` : undefined };
}

/** @returns {Result<Record<Tool, string>>} */
function loadToolPins() {
  try {
    return readToolPins(readFileSync(new URL(`../${PIN_FILE}`, import.meta.url), 'utf8'));
  } catch (error) {
    return { ok: false, error: `cannot read the pin file (${error.code ?? 'unknown error'})` };
  }
}

/** @param {Tool} tool @returns {Result<string>} */
function probeToolVersion(tool) {
  try {
    const output = execFileSync(tool, ['--version'], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
      timeout: VERSION_TIMEOUT_MS,
      maxBuffer: VERSION_MAX_BYTES,
      killSignal: 'SIGKILL',
    });
    return parseToolVersion(tool, output);
  } catch (error) {
    if (error.code === 'ENOENT') {
      const message = tool === 'wasm-opt'
        ? 'wasm-opt is absent from PATH; wasm-pack uses its bundled wasm-opt, not the CI version'
        : 'wasm-pack is absent from PATH or cannot run without a shell';
      return { ok: false, error: message };
    }
    if (error.code === 'ETIMEDOUT') return { ok: false, error: `${tool} --version timed out` };
    if (typeof error.status === 'number') {
      return { ok: false, error: `${tool} --version exited with status ${error.status}` };
    }
    return { ok: false, error: `could not run ${tool} --version (${error.code ?? 'unknown error'})` };
  }
}

/** @param {boolean} strict @param {string[]} args @returns {Decision} */
function checkTools(strict, args) {
  if (args.length !== Number(strict)) return failure(strict, ['expected no options or --strict']);
  const pins = loadToolPins();
  if (!pins.ok) return failure(strict, [pins.error]);
  return decideToolCheck(strict, pins.value, {
    'wasm-pack': probeToolVersion('wasm-pack'),
    'wasm-opt': probeToolVersion('wasm-opt'),
  });
}

function isMainModule() {
  const entry = process.argv[1];
  if (!entry) return false;
  if (pathToFileURL(entry).href === import.meta.url) return true;
  try {
    return pathToFileURL(realpathSync(entry)).href === import.meta.url;
  } catch {
    return false;
  }
}

if (isMainModule()) {
  const args = process.argv.slice(2);
  const strict = args.includes('--strict');
  if (strict) console.log(`Node ${process.version}`);
  const decision = checkTools(strict, args);
  if (decision.stdout) console.log(decision.stdout);
  if (decision.stderr) console.error(decision.stderr);
  process.exitCode = decision.exitCode;
}
