# Autobeat Orchestrator Analysis Report

**Date:** 2026-05-13
**Autobeat Version:** 1.5.2
**Project:** MDS Compiler (`/home/dean/workspace/mds`, branch `feat/compiler`)
**Orchestration ID:** `orchestrator-f59f84c2-7a91-446a-8e4f-0e0b65e0b689`
**Loop ID:** `loop-69f08475-a7ef-4916-bd10-46c4767d90d7`

## Executive Summary

An Autobeat orchestration ran for ~10.9 hours (2026-05-12 20:21 UTC to 2026-05-13 07:13 UTC), completing 28 iterations before manual cancellation. The goal was to implement the MDS compiler from `spec.md`. The orchestrator produced 82 commits, 185+ tests, and a fully working compiler — but it could not self-terminate due to a broken state file mechanism, and ran ~14 iterations past meaningful progress.

This report identifies 5 issues with root cause analysis and actionable recommendations.

## Run Summary

| Metric | Value |
|--------|-------|
| Strategy | RETRY loop (sequential, `freshContext: true`) |
| Eval mode | `shell` (runs a JS completion-check script) |
| Max iterations | 50 |
| Iterations completed | 28 (+1 cancelled) |
| Avg iteration duration | 21.6 minutes |
| Total commits | 82 |
| Lines added/removed | +5,834 / -929 across 74 files |
| Final test count | 185+ (52 unit, 109 integration, 18 acceptance) |
| Snyk issues | 0 across all scans |
| Estimated cost | ~$266 ($9.50/iteration avg) |

## Issue 1: State File Termination Mechanism Is Broken (Critical)

### What happened

The orchestrator state file was **never updated** across 28 iterations. Both orchestrations on this machine (IDs `90ccff97` and `670ed4fb`) show identical initial state:

```json
{
  "version": 1,
  "status": "planning",
  "plan": [],
  "context": {},
  "iterationCount": 0
}
```

The completion check script (`check-complete-state-*.js`) reads this file and exits 0 only if `status === 'complete'`. Since the status never changed, the script always returned exit code 1, and the loop never self-terminated.

### Root cause

The orchestrator's system prompt instructs:
> "Read this file at the START of every iteration. Write updated state BEFORE exiting each iteration."

But the orchestrator runs as a `claude --print` worker task — a restricted, non-interactive session. File writes to `~/.autobeat/orchestrator-state/` from within this context either fail silently or don't persist. There is no feedback mechanism to tell the orchestrator whether its write succeeded.

This is a **design bug in Autobeat**, not a prompt execution failure. The architecture creates a loop whose exit condition depends on file writes that the worker cannot reliably perform.

### Recommendations

- **A) Replace file-based state with a CLI command.** Add `beat state update <orchestration-id> --status complete` so the orchestrator calls a beat command instead of writing files directly. This goes through Autobeat's own persistence layer.
- **B) Use `--eval-mode agent` instead of `--eval-mode shell`.** Have an evaluator agent judge "has the goal been achieved?" by examining the git diff and test results — not by checking a file the worker wrote.
- **C) Add a database-backed state API** accessible to workers, bypassing the filesystem entirely.

## Issue 2: No Convergence Detection (High)

### What happened

The MDS compiler was feature-complete by iteration 7. By iteration 14, the evaluator declared it "production-ready" with 195 passing tests. But the loop continued for 14 more iterations, following this cycle:

1. Re-run the full quality pipeline (Validator → Simplifier → Scrutinizer → Tester → Snyk)
2. Pipeline reports: "all gates pass"
3. Agent says: "ready when you are"
4. No user response → auto-queue next iteration

Late iterations produced minimal value:

| Range | Commits | LOC changed | Nature |
|-------|---------|-------------|--------|
| Iter 1→2 | 18 files | +695/-371 | Deep refactoring, new features |
| Iter 12→19 | ~12 files | +725/-195 | Mostly style (`is_some_and`, iterator idioms) |
| Iter 24→26 | ~8 files | +443/-148 | Re-discovered edge cases, formatting |

Turn counts confirm the pattern:
- Early iterations: 55, 27, 20 turns (agents actively working)
- Late iterations: 3-7 turns (agents saying "done")

### Root cause

The loop has no convergence awareness. Its only termination signals are `maxIterations` (50) and the broken state file check. There is no tracking of:
- Git diff size between iterations
- Test count stability
- Iteration duration / turn count trends

### Recommendations

- Track git diff size between iterations. If 3 consecutive iterations produce < N lines of meaningful change, prompt the evaluator to decide "done or stuck?"
- Track test count stability. If the count hasn't changed in 3 iterations, signal convergence.
- Track iteration duration. Iterations under 5 minutes with < 5 turns are a "nothing to do" signal.
- Add a `--convergence-threshold` flag: "stop if the last N iterations produced no meaningful change."

## Issue 3: Fresh Context Erases Accumulated Knowledge (High)

### What happened

Each iteration runs with `freshContext: true`, so the orchestrator starts every iteration from zero. It reads the state file (which is empty/initial), has no memory of what previous iterations accomplished, and re-discovers the project state from scratch.

This led to:
- Repeated quality gate runs on identical code
- Re-discovery of the same edge cases across iterations
- No awareness that a previous iteration already fixed a given issue
- Iterations 24-26 "finding" security issues that were variations of things addressed in iterations 4-7

### Root cause

`freshContext: true` is appropriate for preventing context window overflow, but without a working state file or iteration summary, the agent loses all accumulated progress knowledge.

### Recommendations

- **A) Fix the state file** (see Issue 1) so each iteration at least knows its iteration number and what was accomplished.
- **B) Use checkpoint-based context:** pass a summary of the previous iteration's output to the next iteration. Autobeat already has `--continue-from` for tasks — apply this pattern to loop iterations.
- **C) Auto-inject context:** have the loop automatically prepend `git log --oneline -10` and `git diff --stat HEAD~5` to each iteration's prompt, giving the agent immediate awareness of recent changes.

## Issue 4: Evaluator Output Protocol Is Ambiguous (Medium)

### What happened

The evaluator produced natural language like:
> "Verdict: Production-ready for v0.1. No push performed — you decide when to push and merge."
> "All quality gates passed... ready when you are."

The retry strategy expects a structured `PASS`/`FAIL` signal as the last line of output. But the orchestrator's evaluator outputs prose, and the loop appears to interpret any non-`PASS` output as "continue."

### Root cause

The system prompt doesn't enforce the `PASS`/`FAIL` protocol for the evaluator. The orchestrator agent treats the evaluation as a report, not a gate.

### Recommendations

- Define a clear protocol in the system prompt: the orchestrator's final output line must be exactly `PASS` (goal achieved, stop) or `FAIL` (continue iterating).
- Add validation in the loop runner: if the last line is neither `PASS` nor `FAIL`, log a warning and treat it as `FAIL` (safe default), but surface this to the dashboard so the user can see the protocol violation.

## Issue 5: No Cost Visibility or Ceiling (Low)

### What happened

28 iterations at ~$9.50 each consumed an estimated ~$266. There was no visibility into cumulative cost during execution and no spending ceiling.

### Recommendations

- Show cumulative cost in `beat orchestrate status`.
- Add a `--max-cost` flag to set a spending ceiling.
- Log per-iteration cost in the dashboard log.
- Consider a cost-efficiency metric: $/LOC-changed or $/test-added to flag when spend is no longer productive.

## Non-Issue: No Git Worktrees Used

The system prompt recommends worktrees for parallel worker isolation:
> "For parallel repository work, instruct workers to create git worktrees"

This instruction is correctly scoped — it applies to `beat run` parallel delegation, not to sequential RETRY loops. Since `beat orchestrate` creates a sequential loop (one agent at a time, via `LoopStrategy.RETRY`), worktrees were unnecessary. All 82 commits went to `feat/compiler` sequentially, with no risk of conflicts.

The worktree instruction is appropriate to keep in the generic system prompt — it's just not applicable to this execution pattern.

## Architecture Summary

```
beat orchestrate "goal"
  └── Creates LoopStrategy.RETRY loop
       └── Iteration 1: claude --print (freshContext, ~20min)
            ├── Reads state file (always sees initial state)
            ├── Does implementation work, commits to branch
            └── Exits (state file unchanged)
       └── Exit condition: node check-complete-state.js → exit 1 (always fails)
       └── Iteration 2: claude --print (freshContext, ~20min)
            └── ... (same pattern, no memory of iteration 1)
       └── ...
       └── Iteration 29: cancelled by user via dashboard
```

The fundamental tension: the architecture uses **stateless workers** (freshContext) with a **stateful termination condition** (state file), but provides no reliable bridge between the two.

## What Went Right

Despite the issues above, the orchestrator produced genuinely good output:
- A fully spec-compliant MDS compiler
- 185+ tests with comprehensive coverage
- 0 security issues (Snyk verified)
- Clean clippy, clean formatting
- Production-ready CLI and library API
- Security hardening (path traversal prevention, file size limits, recursion depth limits)

The first 12 iterations were highly productive. The quality pipeline (Validator → Simplifier → Scrutinizer → Evaluator → Tester → Snyk) is well-designed. The core orchestration concept works — it just needs better termination, convergence, and context-passing mechanics.
