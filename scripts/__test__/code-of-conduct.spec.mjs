/**
 * Tests for CODE_OF_CONDUCT.md (AC-1, AC-2 — issue #38)
 *
 * Verifies that the committed Code of Conduct is genuine Contributor Covenant 2.1
 * text with only the maintainer contact substituted, using an offline fixture so
 * no network access is required at test time.
 *
 * applies ADR-009, avoids PF-013: the sha256 digest anchors the fixture to the
 * upstream source — asserting only its length would not catch content tampering.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, statSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(fileURLToPath(import.meta.url), '../../..');
const FIXTURES = join(ROOT, 'scripts/__test__/fixtures');

// ---------------------------------------------------------------------------
// AC-1, AC-2: Code of Conduct fixture verification
// ---------------------------------------------------------------------------
describe('AC-1 AC-2: Code of Conduct verification', () => {
  // D-COC2: the fixture is the recorded UPSTREAM text, and the sha256 below is
  // the whole point of recording it — without a pinned digest, "CODE_OF_CONDUCT.md
  // differs from the fixture in exactly one line" can be satisfied by editing the
  // fixture. `hash.length === 64` is true of every sha256 ever computed and
  // asserts nothing (applies ADR-009, avoids PF-013).
  //
  // Provenance — reproducible derivation (a reviewer can re-derive FIXTURE_SHA256
  // independently without trusting this file alone):
  //
  //   Source URL (Contributor Covenant 2.1):
  //     https://raw.githubusercontent.com/EthicalSource/contributor_covenant/
  //       release/content/version/2/1/code_of_conduct.md
  //
  //   The upstream file carries a Hugo TOML front-matter block (+++ ... +++) as
  //   site metadata followed by a blank line before the document body.  Strip both
  //   and hash the remainder:
  //
  //     URL='https://raw.githubusercontent.com/EthicalSource/contributor_covenant/release/content/version/2/1/code_of_conduct.md'
  //     curl -sL "$URL" \
  //       | awk '/^\+\+\+$/{c++; if(c==2){emit=1}; next} emit && !started && /^$/{next} emit{started=1; print}' \
  //       | shasum -a 256
  //     # Expected: 369bf7301883368fc19203bd0f1233fed2b83f0378ad19c4d0708bf61925339b
  //
  //   Verified 2026-08-13: the command above produces FIXTURE_SHA256 (5478 bytes).
  //
  //   Historical reference — upstream unstripped digest at plan-authoring time
  //   (the plan recorded sha256 977d781349351fd7c1f076e4c7dc7de2a05b40e12c773542c3815dd4ce7f37ba,
  //   5480 bytes; the upstream body has since changed — 5579 bytes unstripped as of
  //   2026-08-13 — but the stripped body matches the fixture exactly).
  //
  //   If re-running the derivation command above produces a hash other than
  //   FIXTURE_SHA256, the upstream body has changed; review the diff and update
  //   the fixture and this comment if the change is legitimate.
  const FIXTURE_SHA256 = '369bf7301883368fc19203bd0f1233fed2b83f0378ad19c4d0708bf61925339b';
  const FIXTURE_BYTES = 5478;

  test('fixture matches its recorded sha256 and byte count exactly', () => {
    const fixturePath = join(FIXTURES, 'contributor-covenant-2.1.md');
    const buf = readFileSync(fixturePath);
    const hash = createHash('sha256').update(buf).digest('hex');
    const size = statSync(fixturePath).size;
    assert.equal(size, FIXTURE_BYTES, `fixture must be exactly ${FIXTURE_BYTES} bytes; got ${size}`);
    assert.equal(hash, FIXTURE_SHA256,
      'fixture no longer matches the recorded upstream digest — the vendored Contributor ' +
      'Covenant text was modified; restore it rather than updating this constant');
  });

  test('CODE_OF_CONDUCT.md differs from fixture in exactly one line (contact substitution)', () => {
    const cocPath = join(ROOT, 'CODE_OF_CONDUCT.md');
    const fixturePath = join(FIXTURES, 'contributor-covenant-2.1.md');
    const coc = readFileSync(cocPath, 'utf8');
    const fixture = readFileSync(fixturePath, 'utf8');

    const cocLines = coc.split('\n');
    const fixtureLines = fixture.split('\n');

    // Find differing lines
    const maxLen = Math.max(cocLines.length, fixtureLines.length);
    const diffs = [];
    for (let i = 0; i < maxLen; i++) {
      if (cocLines[i] !== fixtureLines[i]) {
        diffs.push({ lineNo: i + 1, coc: cocLines[i], fixture: fixtureLines[i] });
      }
    }

    assert.equal(diffs.length, 1,
      `CODE_OF_CONDUCT.md must differ from fixture in exactly 1 line; got ${diffs.length} diff(s): ` +
      JSON.stringify(diffs));
    assert.ok(diffs[0].coc.includes('deanshrn@gmail.com'),
      `the differing line must contain 'deanshrn@gmail.com'; got: ${diffs[0].coc}`);
    assert.ok(
      (diffs[0].fixture ?? '').includes('[INSERT CONTACT METHOD]'),
      `fixture's differing line must contain '[INSERT CONTACT METHOD]'; got: ${diffs[0].fixture}`
    );
  });

  test('CODE_OF_CONDUCT.md does not contain [INSERT CONTACT METHOD]', () => {
    const coc = readFileSync(join(ROOT, 'CODE_OF_CONDUCT.md'), 'utf8');
    assert.ok(!coc.includes('[INSERT CONTACT METHOD]'),
      'CODE_OF_CONDUCT.md must not contain [INSERT CONTACT METHOD]');
    assert.ok(coc.includes('deanshrn@gmail.com'),
      'CODE_OF_CONDUCT.md must contain the contact email');
  });
});
