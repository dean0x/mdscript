import systemPrompt, { metadata } from './prompts/system.mds';

console.log('=== Prompt (first 100 chars) ===');
const promptText = typeof systemPrompt === 'string' ? systemPrompt : JSON.stringify(systemPrompt, null, 2);
console.log(promptText.substring(0, 100) + '...');
console.log('\n=== Metadata ===');
console.log('Warnings:', metadata.warnings);
console.log('Dependencies:', metadata.dependencies);
console.log('Dependency count:', metadata.dependencies.length);

export { systemPrompt, metadata };
