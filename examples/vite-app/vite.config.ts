import { defineConfig } from 'vite';
import mdsPlugin from '@mdscript/vite-plugin';

export default defineConfig({
  plugins: [
    mdsPlugin({ vars: { env: 'production', debug: false, mode: 'vite-build' } }),
  ],
  build: {
    lib: {
      entry: './src/main.ts',
      formats: ['es'],
      fileName: 'main',
    },
    outDir: 'dist',
    emptyOutDir: true,
  },
});
