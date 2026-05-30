import { init, compile, compileFile, check, checkFile, getBackend, isMdsError } from '@mdscript/mds';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const r = (p) => resolve(__dirname, p);

let passed = 0;
let failed = 0;
const failures = [];

function assert(condition, name, detail) {
  if (condition) {
    passed++;
    console.log(`  PASS: ${name}`);
  } else {
    failed++;
    const msg = detail ? `${name} — ${detail}` : name;
    failures.push(msg);
    console.log(`  FAIL: ${msg}`);
  }
}

function assertContains(output, substring, name) {
  assert(output.includes(substring), name, `expected output to contain "${substring}"`);
}

function assertNotContains(output, substring, name) {
  assert(!output.includes(substring), name, `expected output NOT to contain "${substring}"`);
}

function bodyOf(output) {
  const end = output.indexOf('\n---', 1);
  return end === -1 ? output : output.slice(end + 4);
}

async function run() {
  console.log('Initializing MDS...');
  await init();
  const backend = getBackend();
  console.log(`Backend: ${backend}\n`);

  // ── Group 1: Library files (function-only, empty body) ──
  console.log('── Library files ──');
  {
    const result = await compileFile(r('lib/formatting.mds'));
    assert(bodyOf(result.output).trim() === '', 'formatting.mds compiles to empty body');
    assert(result.warnings.length === 0, 'formatting.mds no warnings');
  }
  {
    const result = await compileFile(r('lib/guardrails.mds'));
    assert(bodyOf(result.output).trim() === '', 'guardrails.mds compiles to empty body');
  }
  {
    const result = await compileFile(r('lib/personas.mds'));
    assert(bodyOf(result.output).trim() === '', 'personas.mds compiles to empty body');
  }
  {
    const result = await compileFile(r('lib/examples.mds'));
    assert(bodyOf(result.output).trim() === '', 'examples.mds compiles to empty body');
  }

  // ── Group 2: Shared utilities ──
  console.log('\n── Shared utilities ──');
  {
    const result = await compileFile(r('shared/tool-registry.mds'));
    assertContains(result.output, '**code_exec** (Premium)', 'tool-registry premium conditional');
    assertContains(result.output, '**file_read**:', 'tool-registry basic tool');
  }
  {
    const result = await compileFile(r('shared/chain-consumer.mds'));
    assertContains(result.output, '**Testing 3-level import chain**', 'chain-consumer bold');
    assertContains(result.output, 'Safety Guidelines', 'chain-consumer re-exported safety_rules');
    assertContains(result.output, 'Algebra teacher', 'chain-consumer re-exported teacher');
    assert(result.dependencies.length > 0, 'chain-consumer has dependencies');
  }

  // ── Group 3: Agents ──
  console.log('\n── Agent files ──');
  {
    const result = await compileFile(r('agents/data-analyst.mds'));
    assertContains(result.output, '**DataInsight Pro**', 'data-analyst bold agent name');
    assertContains(result.output, '**Version**: `2.1`', 'data-analyst badge');
    assertContains(result.output, '`SQL query generation`', 'data-analyst capability loop');
    assertContains(result.output, '**sql_runner** (query) — Safe', 'data-analyst tool safe check');
    assertContains(result.output, '**data_export** (export) — Requires approval', 'data-analyst tool unsafe check');
    assertContains(result.output, '`analytics`', 'data-analyst nested dot-path schema');
    assertNotContains(result.output, 'Debug Mode', 'data-analyst debug=false hides debug section');
    assert(result.dependencies.length >= 2, `data-analyst has ${result.dependencies.length} deps`);
  }
  {
    const result = await compileFile(r('agents/code-reviewer.mds'));
    assertContains(result.output, '**CodeReview Bot**', 'code-reviewer bold name');
    assertContains(result.output, '`critical` — Must fix before merge', 'code-reviewer conditional severity');
    assertContains(result.output, '**Type safety**', 'code-reviewer focus area');
    assert(result.warnings.length > 0, 'code-reviewer warns about empty include');
  }
  {
    const result = await compileFile(r('agents/orchestrator.mds'));
    assertContains(result.output, '**AgentOrch**', 'orchestrator bold name');
    assertContains(result.output, '**DataInsight Pro** — Role: `analyst`', 'orchestrator active agent');
    assertContains(result.output, '~~Legacy Scanner~~', 'orchestrator inactive agent');
    assertNotContains(result.output, '~~DataInsight Pro~~', 'orchestrator active not in inactive list');
    assertContains(result.output, 'Running up to 3 agents concurrently', 'orchestrator concurrent check');
    assertContains(result.output, 'tasks are queued', 'orchestrator fallback strategy');
  }

  // ── Group 4: Edge cases ──
  console.log('\n── Edge cases ──');
  {
    const result = await compileFile(r('edge/code-passthrough.mds'));
    assertContains(result.output, 'return {"item_id": item_id', 'code-passthrough no interpolation in code');
    assertContains(result.output, 'After the code block, interpolation resumes: Python', 'code-passthrough resumes after code');
    assertContains(result.output, '"name": "test"', 'code-passthrough json block preserved');
    assertContains(result.output, '**Stack**: `Python`', 'code-passthrough final badge');
  }
  {
    const result = await compileFile(r('edge/escaped-braces.mds'));
    assertContains(result.output, '{not_a_var}', 'escaped-braces literal braces');
    assertContains(result.output, 'INTERPOLATED', 'escaped-braces real var');
    assertContains(result.output, '{literal} then INTERPOLATED', 'escaped-braces mixed line');
  }
  {
    const result = await compileFile(r('edge/deep-nesting.mds'));
    const body = bodyOf(result.output);
    assertContains(body, '## Engineering', 'deep-nesting active dept');
    assertContains(body, '**Bob** (Senior)', 'deep-nesting 4-level deep senior check');
    assertContains(body, '- Carol', 'deep-nesting non-senior');
    assertNotContains(body, 'Marketing', 'deep-nesting inactive dept hidden');
    assertNotContains(body, 'Grace', 'deep-nesting inactive member hidden');
  }
  {
    const result = await compileFile(r('edge/falsy-matrix.mds'));
    assertNotContains(result.output, 'FAIL', 'falsy-matrix all PASS, no FAIL');
    const passCount = (result.output.match(/PASS/g) || []).length;
    assert(passCount === 15, `falsy-matrix has ${passCount}/15 PASS assertions`);
  }
  {
    const result = await compileFile(r('edge/shadowing-stress.mds'));
    assertContains(result.output, 'Before anything: outer_value', 'shadowing outer initial');
    assertContains(result.output, 'Function sees: func_arg', 'shadowing fn param');
    assertContains(result.output, 'After function: outer_value', 'shadowing restored after fn');
    assertContains(result.output, 'Loop sees: first', 'shadowing loop var');
    assertContains(result.output, 'After loop: outer_value', 'shadowing restored after loop');
    assertContains(result.output, 'Final outer: outer_value', 'shadowing final check');
  }
  {
    const result = await compileFile(r('edge/empty-collections.mds'));
    assertNotContains(result.output, 'FAIL', 'empty-collections no FAIL');
    assertContains(result.output, 'Non-empty works: test', 'empty-collections label preserved');
    assertContains(result.output, '- one', 'empty-collections filled array works');
  }

  // ── Group 5: Main entry point ──
  console.log('\n── Main entry point ──');
  {
    const result = await compileFile(r('main.mds'));
    assertContains(result.output, '**AI Agent Factory**', 'main bold project name');
    assertContains(result.output, 'Safety Guidelines', 'main safety_rules from barrel');
    assertContains(result.output, '`DataInsight Pro`', 'main enabled_agents loop');
    assertContains(result.output, '**DataInsight Pro** Configuration', 'main @include analyst');
    assertContains(result.output, '**AgentOrch**', 'main @include orchestrator');
    assertContains(result.output, '{not_interpolated}', 'main escaped braces');
    assertContains(result.output, '@if condition:', 'main code block passthrough');
    assertContains(result.output, 'Running in production mode', 'main debug=false conditional');
    assert(result.dependencies.length >= 7, `main has ${result.dependencies.length} transitive deps (expected 7+)`);
  }

  // ── Group 6: Runtime variable overrides ──
  console.log('\n── Runtime vars ──');
  {
    const result = await compileFile(r('main.mds'), {
      vars: { debug: true, environment: 'staging', version: '2.0-rc1' },
    });
    assertContains(result.output, 'v2.0-rc1', 'vars override version');
    assertContains(result.output, '`staging`', 'vars override environment');
    assertContains(result.output, 'Debug mode is ON', 'vars override debug to true');
  }
  {
    const result = await compileFile(r('main.mds'), {
      vars: { debug: true },
    });
    assertContains(result.output, 'v1.0', 'minimal vars keep original version');
    assertContains(result.output, 'Debug mode is ON', 'minimal vars debug override');
  }

  // ── Group 7: compile() inline templates ──
  console.log('\n── Inline compile() ──');
  {
    const result = compile('Hello {name}!', { vars: { name: 'World' } });
    assert(result.output.trim() === 'Hello World!', 'inline simple interpolation');
  }
  {
    const src = [
      '---',
      'items:',
      '  - alpha',
      '  - beta',
      '---',
      '@for item in items:',
      '- {item}',
      '@end',
    ].join('\n');
    const result = compile(src);
    assertContains(result.output, '- alpha', 'inline loop alpha');
    assertContains(result.output, '- beta', 'inline loop beta');
  }
  {
    const src = [
      '---',
      'debug: true',
      '---',
      '@if debug:',
      'DEBUG ON',
      '@else:',
      'DEBUG OFF',
      '@end',
    ].join('\n');
    const result = compile(src);
    assertContains(result.output, 'DEBUG ON', 'inline truthy conditional');
  }
  {
    const src = [
      '@define greet(name):',
      'Hello {name}!',
      '@end',
      '{greet("World")}',
    ].join('\n');
    const result = compile(src);
    assertContains(result.output, 'Hello World!', 'inline function call');
  }
  {
    const src = 'Escaped: \\{literal\\} and plain text.';
    const result = compile(src);
    assertContains(result.output, '{literal}', 'inline escaped braces');
  }

  // ── Group 8: check() and checkFile() ──
  console.log('\n── Validation (check) ──');
  {
    const checkResult = await checkFile(r('edge/shadowing-stress.mds'));
    assert(Array.isArray(checkResult.warnings), 'checkFile shadowing returns warnings array');
  }
  {
    const result = check('Hello {name}!', { vars: { name: 'Test' } });
    assert(Array.isArray(result.warnings), 'check inline returns warnings array');
  }
  {
    try {
      check('{undefined_thing}');
      assert(false, 'check rejects undefined var');
    } catch (err) {
      assert(isMdsError(err), 'check undefined throws MdsError');
    }
  }

  // ── Group 9: Error handling with isMdsError ──
  console.log('\n── Error handling ──');
  {
    try {
      await compileFile(r('errors/bad-circular-a.mds'));
      assert(false, 'circular import should throw');
    } catch (err) {
      assert(isMdsError(err), 'circular import is MdsError');
      assert(err.code === 'mds::circular_import', `circular error code: ${err.code}`);
    }
  }
  {
    try {
      await compileFile(r('errors/bad-arity.mds'));
      assert(false, 'arity mismatch should throw');
    } catch (err) {
      assert(isMdsError(err), 'arity error is MdsError');
      assert(err.code === 'mds::arity', `arity error code: ${err.code}`);
    }
  }
  {
    try {
      await compileFile(r('errors/bad-undefined.mds'));
      assert(false, 'undefined var should throw');
    } catch (err) {
      assert(isMdsError(err), 'undefined var is MdsError');
      assert(err.code === 'mds::undefined_var', `undefined error code: ${err.code}`);
    }
  }
  {
    try {
      await compileFile(r('errors/bad-type.mds'));
      assert(false, 'type error should throw');
    } catch (err) {
      assert(isMdsError(err), 'type error is MdsError');
      assert(err.code === 'mds::type_error', `type error code: ${err.code}`);
    }
  }
  {
    assert(!isMdsError(new Error('generic')), 'isMdsError rejects generic Error');
    assert(!isMdsError('string'), 'isMdsError rejects non-Error');
    assert(!isMdsError(null), 'isMdsError rejects null');
  }

  // ── Group 10: Dependencies tracking ──
  console.log('\n── Dependencies ──');
  {
    const result = await compileFile(r('shared/chain-consumer.mds'));
    const deps = result.dependencies;
    const hasReexport = deps.some(d => d.includes('reexport-chain.mds'));
    assert(hasReexport, 'chain-consumer deps include reexport-chain.mds');
    const hasFormatting = deps.some(d => d.includes('formatting.mds'));
    assert(hasFormatting, 'chain-consumer deps include formatting.mds (transitive through re-export)');
  }
  {
    const result = await compileFile(r('edge/code-passthrough.mds'));
    const deps = result.dependencies;
    assert(deps.length >= 1, `code-passthrough has ${deps.length} deps (formatting.mds)`);
  }
  {
    const result = await compileFile(r('main.mds'));
    const deps = result.dependencies;
    assert(deps.length >= 7, `main.mds has ${deps.length} transitive deps (expected 7+)`);
    const hasGuardrails = deps.some(d => d.includes('guardrails.mds'));
    assert(hasGuardrails, 'main deps include guardrails.mds');
    const hasOrchestrator = deps.some(d => d.includes('orchestrator.mds'));
    assert(hasOrchestrator, 'main deps include orchestrator.mds');
    const hasFormatting = deps.some(d => d.includes('formatting.mds'));
    assert(hasFormatting, 'main deps include formatting.mds');
  }

  // ── Summary ──
  console.log('\n══════════════════════════════════════');
  console.log(`  Results: ${passed} passed, ${failed} failed`);
  console.log('══════════════════════════════════════');
  if (failures.length > 0) {
    console.log('\nFailures:');
    for (const f of failures) {
      console.log(`  - ${f}`);
    }
  }

  process.exit(failed > 0 ? 1 : 0);
}

run().catch((err) => {
  console.error('FATAL:', err);
  process.exit(2);
});
