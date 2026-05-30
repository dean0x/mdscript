/**
 * Shared test helpers for @mdscript/mds tests.
 */
import { fileURLToPath } from 'node:url';
import path from 'node:path';

export const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const FIXTURES = path.join(__dirname, 'fixtures');
export const SIMPLE_MDS = path.join(FIXTURES, 'simple.mds');
export const IMPORT_PROVIDER_MDS = path.join(FIXTURES, 'import_provider.mds');
export const IMPORT_CONSUMER_MDS = path.join(FIXTURES, 'import_consumer.mds');
export const ENTRY_MDS = path.join(FIXTURES, 'imports', 'entry.mds');
export const EMPTY_MDS = path.join(FIXTURES, 'edge', 'empty.mds');
export const FRONTMATTER_ONLY_MDS = path.join(FIXTURES, 'edge', 'frontmatter_only.mds');
export const MD_EXTENSION = path.join(FIXTURES, 'edge', 'md_extension.md');
