import codePassthrough from '../../stress-test/edge/code-passthrough.mds';
import escapedBraces from '../../stress-test/edge/escaped-braces.mds';
import shadowingStress from '../../stress-test/edge/shadowing-stress.mds';
import emptyCollections from '../../stress-test/edge/empty-collections.mds';
import dataAnalyst from '../../stress-test/agents/data-analyst.mds';
import codeReviewer from '../../stress-test/agents/code-reviewer.mds';
import orchestrator from '../../stress-test/agents/orchestrator.mds';
import deepNesting from '../../stress-test/edge/deep-nesting.mds';
import falsyMatrix from '../../stress-test/edge/falsy-matrix.mds';
import chainConsumer from '../../stress-test/shared/chain-consumer.mds';
import main from '../../stress-test/main.mds';

console.log('=== Stress Test: Code Passthrough ===');
console.log(codePassthrough);

console.log('\n=== Stress Test: Escaped Braces ===');
console.log(escapedBraces);

console.log('\n=== Stress Test: Shadowing Stress ===');
console.log(shadowingStress);

console.log('\n=== Stress Test: Empty Collections ===');
console.log(emptyCollections);

console.log('\n=== Stress Test: Data Analyst ===');
console.log(dataAnalyst);

console.log('\n=== Stress Test: Code Reviewer ===');
console.log(codeReviewer);

console.log('\n=== Stress Test: Orchestrator ===');
console.log(orchestrator);

console.log('\n=== Stress Test: Deep Nesting ===');
console.log(deepNesting);

console.log('\n=== Stress Test: Falsy Matrix ===');
console.log(falsyMatrix);

console.log('\n=== Stress Test: Chain Consumer ===');
console.log(chainConsumer);

console.log('\n=== Stress Test: Main ===');
console.log(main);

export { codePassthrough, escapedBraces, shadowingStress, emptyCollections, dataAnalyst, codeReviewer, orchestrator, deepNesting, falsyMatrix, chainConsumer, main };
