/**
 * Browser entry for the live HMR demo (`npm run dev`).
 *
 * Imports a compiled MDS prompt and renders it into the page. The
 * @mdscript/vite-plugin triggers a full-page reload whenever the imported
 * `.mds` file — or any of its transitive `@import` dependencies — changes on
 * disk, so editing a prompt and saving updates what you see here immediately.
 */
import reviewerPrompt from './prompts/reviewer.mds';

const out = document.getElementById('out');
if (out) out.textContent = reviewerPrompt;

console.log('[mds-demo] compiled prompt rendered (%d chars)', reviewerPrompt.length);
