#!/usr/bin/env node
// Programmatic rspack build — equivalent to `rspack build --config rspack.config.mjs`.
// Uses the JS API directly so no @rspack/cli is needed.
import { rspack } from '@rspack/core';
import config from './rspack.config.mjs';

rspack(config, (err, stats) => {
  if (err) {
    console.error(err.stack || err);
    process.exit(1);
  }
  if (stats.hasErrors()) {
    console.error(stats.toString({ errors: true, warnings: false }));
    process.exit(1);
  }
  console.log(stats.toString({ colors: true, assets: true, timings: true }));
});
