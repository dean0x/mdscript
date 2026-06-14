<!-- TL;DR: 6 pitfalls. Key: PF-002, PF-003, PF-004, PF-005, PF-006 -->
# Known Pitfalls

Area-specific gotchas, fragile areas, and past bugs.

## PF-001: GitHub Actions transitive skip propagation silently skips all downstream jobs unless explicit if conditions are set

- **Area**: CI pipeline reliability
- **Issue**: when a job is skipped, GitHub Actions propagates the skip transitively through all needs chains — every downstream job is also skipped unless it has an explicit if condition
- **Impact**: entire release pipeline silently no-ops with no error or warning
- **Resolution**: add explicit if conditions on every job downstream of a conditional step
- **Status**: Active
- **Source**: self-learning:obs_p7q1r5

## PF-002: bump-version script must update path+version workspace crate deps; semver ^0.1.0 does not satisfy 0.2.0

- **Area**: release tooling (scripts/bump-version.mjs, Cargo.toml workspace crates)
- **Issue**: bump-version.mjs only updated [workspace.package] version but not explicit path+version dependencies in mds-cli, mds-napi, and mds-wasm Cargo.toml files
- **Impact**: cargo metadata fails in CI because ^0.1.0 does not satisfy 0.2.0 for pre-1.0 crates, blocking the release pipeline
- **Resolution**: bump-version.mjs must find and update all path+version dep references when bumping the workspace version
- **Status**: Active
- **Source**: self-learning:obs_q8r2s6

## PF-003: npm provenance verification rejects publishes when package.json repository.url does not match the GitHub Actions workflow source repository

- **Area**: npm publish with SLSA provenance (release.yml publish-npm job)
- **Issue**: after renaming the GitHub repo, package.json repository.url fields still referenced the old repo name
- **Impact**: npm provenance verification performs a strict URL match — all publishes fail with attestation mismatch
- **Resolution**: update repository.url in every package.json to the current canonical repository name whenever the repo is renamed
- **Status**: Active
- **Source**: self-learning:obs_r9s3t7

## PF-004: An alternate output/code path can silently bypass a resource limit that is enforced only on the primary path

- **Area**: CLI input handling (mds-cli read path, MAX_FILE_SIZE / 10 MiB cap)
- **Issue**: the markdown output mode enforced MAX_FILE_SIZE through the resolver, but a newly added messages output mode read the input with raw std::fs::read and never went through that resolver, silently bypassing the 10 MiB cap
- **Impact**: the resource-exhaustion protection was real for one output mode and absent for the other — an oversized input would be rejected via markdown mode but accepted via messages mode
- **Resolution**: extract a single shared input-reading function (read_build_input) that enforces MAX_FILE_SIZE once for both modes, and add an oversized-file rejection test. General lesson: when adding a parallel code path, audit which security/resource boundaries the primary path enforced and route the new path through the same enforcement point rather than re-reading at a lower level.
- **Status**: Active
- **Source**: self-learning:obs_v3w7x1

## PF-005: Relocating commits with git reset --hard then git checkout deletes working-tree copies of files that were tracked at that moment but are untracked/ignored on the destination

- **Area**: git working-tree safety during branch/commit relocation (reset --hard + checkout) interacting with a freshly-changed .gitignore tracking policy
- **Issue**: when files are tracked at commit C but the destination branch (or an updated .gitignore) no longer tracks them, git reset --hard followed by git checkout removes their working-tree copies because they are no longer part of the tracked tree on the target
- **Impact**: hundreds of on-disk files (504 docs/ + 5 dream/) silently disappeared from the working tree, contradicting the intent that every file stays on disk as ignored/untracked
- **Resolution**: restore the deleted files from the known-good commit with git restore --worktree <paths> (worktree-only, no re-staging) so they return to disk as ignored. General lesson: when a tracking-policy change (git rm --cached) and a branch/commit relocation (reset/checkout) happen close together, the reset can purge now-untracked files from disk — verify the working tree after the move and restore from history rather than assuming untracked-on-disk files survive a hard reset.
- **Status**: Active
- **Source**: self-learning:obs_a8b2c6

## PF-006: A file watcher that recompiles on filesystem events self-triggers an infinite loop when compilation reads the watched file and that read itself emits an event (Linux inotify Access)

- **Area**: mds watch event loop, cross-platform notify backend (Linux inotify vs macOS FSEvents)
- **Issue**: on Linux, inotify reports read-access (IN_ACCESS -> EventKind::Access) events. The watcher recompiles a .mds on any event, and compilation opens/reads the watched source, which emits an Access event that retriggers compilation — an unbounded self-feedback loop (2846 errors observed in CI). It was invisible on macOS because FSEvents does not surface read-access as a change event, so it only manifested in the Linux CI job. The (mtime,size) change gate did not stop it because Access events arrive with the file unchanged yet still drive the event handler
- **Impact**: a Linux-only busy-loop that spams thousands of recompiles/errors and would peg CPU for any user on Linux
- **Resolution**: filter out EventKind::Access(_) at the event-intake boundary so reads never count as change events (fix 6b7f2fe, PR #57). General lesson: a watcher that reads the files it watches must exclude read/access event classes, and platform watcher backends differ in which event classes they emit — test the watch loop on every target OS, not just the development platform.
- **Status**: Active
- **Source**: self-learning:obs_c0d4e8
