import { dirname, basename } from 'node:path/posix';

// Publishable npm packages (host napi package, universal, wasm and bundlers).
export const PKG_PATHS = Object.freeze([
  'crates/mds-napi/package.json',
  'packages/mds/package.json',
  'packages/mds-wasm/package.json',
  'packages/bundler-utils/package.json',
  'packages/vite-plugin/package.json',
  'packages/rollup-plugin/package.json',
  'packages/webpack-loader/package.json',
  'packages/rspack-loader/package.json',
]);

// Workspace crates with a path and version dependency on mds-core.
export const CRATE_CARGO_PATHS = Object.freeze([
  'crates/mds-cli/Cargo.toml',
  'crates/mds-napi/Cargo.toml',
  'crates/mds-wasm/Cargo.toml',
  'crates/mds-python/Cargo.toml',
]);

export const DEP_SETS = Object.freeze(['dependencies', 'optionalDependencies', 'peerDependencies']);
export const PKG_DIRS = Object.freeze(PKG_PATHS.map(dirname));
export const LOCK_MEMBERS = Object.freeze([...CRATE_CARGO_PATHS.map((path) => basename(dirname(path))), 'mds-core']);
