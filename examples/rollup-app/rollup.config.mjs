import mdsPlugin from '@mdscript/rollup-plugin';
import { nodeResolve } from '@rollup/plugin-node-resolve';

export default {
  input: 'src/main.ts',
  output: {
    file: 'dist/main.mjs',
    format: 'es',
  },
  plugins: [
    mdsPlugin({ vars: { debug: false, mode: 'rollup-build' } }),
    nodeResolve({ extensions: ['.ts', '.js'] }),
  ],
};
