import systemPrompt from './prompts/system.mds';
import reviewerPrompt from './prompts/reviewer.mds';
import v2Features from './prompts/v2-features.mds';
import chatMessages from './prompts/chat.mds';
import { codePassthrough, escapedBraces, shadowingStress, emptyCollections } from './stress';

console.log('=== System Prompt ===');
console.log(systemPrompt);
console.log('\n=== Reviewer Prompt ===');
console.log(reviewerPrompt);
console.log('\n=== v0.2.0 Features ===');
console.log(v2Features);

// AC-API-18: messages template import must be an array (kind='messages')
// Markdown imports (system, reviewer, v2Features) are strings.
// The chat.mds template contains @message blocks, so the bundler plugin emits an array.
if (!Array.isArray(chatMessages)) {
  throw new Error('BUG: chatMessages must be an array — messages template import should yield Message[]');
}
if (typeof systemPrompt !== 'string') {
  throw new Error('BUG: systemPrompt must be a string — markdown template import should yield string');
}
console.log('\n=== Chat Messages (array) ===');
console.log(JSON.stringify(chatMessages, null, 2));

console.log('\n=== Stress Test: Code Passthrough ===');
console.log(codePassthrough);
console.log('\n=== Stress Test: Escaped Braces ===');
console.log(escapedBraces);
console.log('\n=== Stress Test: Shadowing ===');
console.log(shadowingStress);
console.log('\n=== Stress Test: Empty Collections ===');
console.log(emptyCollections);

export { systemPrompt, reviewerPrompt, v2Features, chatMessages, codePassthrough, escapedBraces, shadowingStress, emptyCollections };
