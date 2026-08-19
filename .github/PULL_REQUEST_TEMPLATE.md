<!--
Thanks for contributing to MDS! Please fill out the checklist below.
PR titles follow Conventional Commits (feat:, fix:, refactor:, chore:, docs:, ...).
-->

## What does this PR do?

<!-- A short description of the change and the motivation behind it. -->

## Related issues

<!-- e.g. Closes #123 -->

## Checklist

- [ ] PR title follows [Conventional Commits](https://www.conventionalcommits.org/)
- [ ] Tests added/updated for the change (and assert behavior, not implementation)
- [ ] `CHANGELOG.md` updated under `## [Unreleased]` (for user-facing changes)
- [ ] Rust gates pass: `cargo test --workspace`, `cargo fmt --all --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] JS gates pass (if touched): `npm run build --workspaces && npm test --workspaces`
- [ ] No new compiler/linter warnings
- [ ] Source hygiene: `node scripts/verify-no-control-bytes.mjs` exits 0
- [ ] **Before any `--admin` merge**: run `node scripts/verify-pr-checks.mjs <pr-number>`
      and use the `gh pr merge --squash --admin --match-head-commit <sha>` command it emits
      (PF-017: a cancelled run reads as green without this check)
