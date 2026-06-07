/**
 * compileMessages() tests for @mdscript/mds universal package.
 * Tests: U-CM1 through U-CM12
 */
import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { compileMessages, isMdsError, init } from '../dist/node.js';

describe('compileMessages', () => {
  before(() => init());

  // ── AC-1: bare-word role ───────────────────────────────────────────────────

  test('U-CM1: compileMessages with bare-word system role', () => {
    const result = compileMessages('@message system:\nYou are helpful.\n@end\n');
    assert.ok(Array.isArray(result.messages), 'messages must be an array');
    assert.equal(result.messages.length, 1, 'should have one message');
    assert.equal(result.messages[0].role, 'system');
    assert.equal(result.messages[0].content, 'You are helpful.');
  });

  test('U-CM2: compileMessages result has correct shape', () => {
    const result = compileMessages('@message user:\nHello.\n@end\n');
    assert.ok(Array.isArray(result.messages));
    assert.ok(Array.isArray(result.warnings));
    assert.ok(Array.isArray(result.dependencies));
  });

  // ── AC-2: dynamic role via runtime vars ───────────────────────────────────

  test('U-CM3: compileMessages with dynamic role from vars', () => {
    const result = compileMessages('@message {r}:\nContent.\n@end\n', {
      vars: { r: 'assistant' },
    });
    assert.equal(result.messages[0].role, 'assistant');
    assert.equal(result.messages[0].content, 'Content.');
  });

  // ── AC-3: no @message blocks → error ─────────────────────────────────────

  test('U-CM4: compileMessages throws MdsError when no @message blocks', () => {
    assert.throws(
      () => compileMessages('Hello world!\n'),
      (err) => {
        assert.ok(isMdsError(err), `must be MdsError, got: ${err}`);
        assert.ok(
          err.message.includes('no @message') || err.message.includes('at least one'),
          `expected no-message-blocks error, got: ${err.message}`,
        );
        return true;
      },
    );
  });

  // ── AC-4: multiple messages in order ─────────────────────────────────────

  test('U-CM5: compileMessages returns messages in source order', () => {
    const src = [
      '@message system:\nSys.\n@end',
      '@message user:\nUsr.\n@end',
      '@message assistant:\nAst.\n@end',
    ].join('\n') + '\n';
    const result = compileMessages(src);
    assert.equal(result.messages.length, 3);
    assert.equal(result.messages[0].role, 'system');
    assert.equal(result.messages[1].role, 'user');
    assert.equal(result.messages[2].role, 'assistant');
  });

  // ── AC-5: interpolation inside message body ───────────────────────────────

  test('U-CM6: compileMessages resolves interpolation inside @message body', () => {
    const src = '---\nname: World\n---\n@message user:\nHello {name}!\n@end\n';
    const result = compileMessages(src);
    assert.equal(result.messages[0].content, 'Hello World!');
  });

  // ── AC-6: content trimming ────────────────────────────────────────────────

  test('U-CM7: compileMessages trims content whitespace', () => {
    const src = '@message system:\n\n  Hello.  \n\n@end\n';
    const result = compileMessages(src);
    assert.equal(result.messages[0].content, 'Hello.');
  });

  // ── AC-7: empty body is skipped ───────────────────────────────────────────

  test('U-CM8: compileMessages skips messages with empty body', () => {
    const src = '@message system:\n   \n@end\n@message user:\nContent.\n@end\n';
    const result = compileMessages(src);
    assert.equal(result.messages.length, 1, 'empty message should be skipped');
    assert.equal(result.messages[0].role, 'user');
  });

  // ── AC-8: orphan text warns ───────────────────────────────────────────────

  test('U-CM9: compileMessages warns for orphan text outside @message', () => {
    const src = 'Orphan text.\n@message user:\nQ?\n@end\n';
    const result = compileMessages(src);
    assert.ok(result.warnings.length > 0, 'expected at least one warning for orphan text');
    const hasOrphanWarn = result.warnings.some(
      (w) => w.includes('outside @message') || w.includes('orphan') || w.includes('ignored'),
    );
    assert.ok(hasOrphanWarn, `expected orphan warning; got: ${JSON.stringify(result.warnings)}`);
  });

  // ── AC-9: nested @message → error ────────────────────────────────────────

  test('U-CM10: compileMessages throws on nested @message blocks', () => {
    const src = '@message system:\n@message user:\nNested.\n@end\n@end\n';
    assert.throws(
      () => compileMessages(src),
      (err) => {
        assert.ok(isMdsError(err), `must be MdsError, got: ${err}`);
        assert.ok(
          err.message.includes('nested') || err.message.includes('cannot be nested'),
          `expected nesting error, got: ${err.message}`,
        );
        return true;
      },
    );
  });

  // ── AC-10: @for generates multiple messages ───────────────────────────────

  test('U-CM11: compileMessages with @for generating multiple messages', () => {
    const src = [
      '---',
      'roles:',
      '  - system',
      '  - user',
      '---',
      '@for role in roles:',
      '@message {role}:',
      'Content for {role}.',
      '@end',
      '@end',
      '',
    ].join('\n');
    const result = compileMessages(src);
    assert.equal(result.messages.length, 2, `got: ${JSON.stringify(result.messages)}`);
    assert.equal(result.messages[0].role, 'system');
    assert.equal(result.messages[1].role, 'user');
  });

  // ── AC-11: bare-word role is a string literal ─────────────────────────────

  test('U-CM12: bare-word role is treated as string literal, not variable lookup', () => {
    // Even with `system` defined as a var, the @message system: role must be "system".
    const src = '---\nsystem: injected\n---\n@message system:\nBody.\n@end\n';
    const result = compileMessages(src);
    assert.equal(
      result.messages[0].role,
      'system',
      `bare-word role must not look up vars; got: ${result.messages[0].role}`,
    );
  });
});
