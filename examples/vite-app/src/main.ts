import systemPrompt from './prompts/system.mds';
import reviewerPrompt from './prompts/reviewer.mds';
import { codePassthrough, escapedBraces, shadowingStress, emptyCollections } from './stress';

console.log('=== System Prompt ===');
console.log(systemPrompt);
console.log('\n=== Reviewer Prompt ===');
console.log(reviewerPrompt);

console.log('\n=== Stress Test: Code Passthrough ===');
console.log(codePassthrough);
console.log('\n=== Stress Test: Escaped Braces ===');
console.log(escapedBraces);
console.log('\n=== Stress Test: Shadowing ===');
console.log(shadowingStress);
console.log('\n=== Stress Test: Empty Collections ===');
console.log(emptyCollections);

export { systemPrompt, reviewerPrompt, codePassthrough, escapedBraces, shadowingStress, emptyCollections };
