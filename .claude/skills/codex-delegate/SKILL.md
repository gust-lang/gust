---
name: codex-delegate
description: Delegate a well-scoped implementation task to the local Codex CLI (`codex exec`), monitor it periodically while it runs, then review its diff before committing. Use when the user asks to hand work to codex, delegate a bug fix to codex, or run codex on an issue.
user-invocable: true
allowed-tools:
  - Read
  - Write
  - Edit
  - Bash
  - Monitor
  - TaskStop
---

# Delegating to Codex

`codex` (OpenAI Codex CLI, `/home/vlad/.local/bin/codex`) is a *separate* agentic coder,
not a Claude subagent — the `Agent` tool cannot reach it. Drive it with `codex exec`.

Arguments passed: `$ARGUMENTS`

Use it for **well-scoped, verifiable** work: a filed bug with a repro, a mechanical
refactor, a migration across many files. Do not use it for design decisions, for anything
touching an open language-design question, or for work you cannot state a pass/fail
verification for.

---

## 1. Preflight

```bash
cd <repo-or-worktree-root>
git status --short          # MUST be clean — you need a clean diff to review afterwards
codex --version
timeout 120 codex exec --sandbox read-only -C "$PWD" "Reply with exactly: READY"
```

A clean tree is not optional. The whole value of this workflow is reviewing exactly what
Codex changed; uncommitted work of your own makes that impossible.

**One at a time.** Two `codex exec` runs against the same worktree will collide — they
edit overlapping files with no coordination. Queue them; write the second brief while the
first runs.

---

## 2. Write the brief to a file

Never pass a long prompt as an argv string — quoting will bite you. Write a markdown file
and pipe it in.

The brief is the whole job. Codex starts cold: it has no idea what you have established
in conversation, which decisions are deliberately open, or what "done" means here.

```markdown
Fix bug #NNN in this Rust codebase (a language interpreter for the Metel language).

## The bug
<one-paragraph statement, then a COPY-PASTEABLE repro with the exact command to run it>

## Cause
<file:line, the offending code quoted, and the adjacent code that does it correctly>

## What to do
<the intended fix, plus any mechanism it should REUSE rather than reinvent>

## Constraints - important
- Do NOT change <X>. It is a deliberately open design question (issue #NNN).
- Do NOT modify anything under `docs/internal/rfcs/` — lifecycle-managed elsewhere.
- You MAY update `docs/public/reference/spec/*.md` if behaviour changes.
- Do not commit anything. Leave changes in the working tree.
- There may be uncommitted changes from a previous task. Leave them alone.

## Verification - all required
Run from `metel-interpreter/`:
  cargo build --release
  cargo test --release                            # must be 100% green
  cargo clippy --release --lib -- -D warnings     # must be clean

NOTE ON TIMING: a release rebuild after touching core files takes SEVERAL MINUTES, and
the test suite another ~90 seconds. This is normal. Do not treat a quiet cargo
invocation as a hang and do not kill it — wait for it to finish.

Add regression fixtures. Conventions:
- Positive: `.mtl` under `tests/integration/sources/<suite>/`, must exit 0, use `assert(...)`.
- Negative in the `typechecking` suite: `// ERROR[T0005]` as a trailing comment ON the
  offending line.
- Negative in the `evaluator` suite: `// TYPECHECK_ERROR[T0002]` on its own line near the top.
- A new fixture defining helper functions must CALL them from `main` — an uncalled
  function silently tests nothing.
See `tests/integration/sources/typechecking/addressability/` for recent examples.

Cover: <enumerate the cases explicitly>

Report at the end: what you changed, what you verified, and anything you found that you
did NOT fix.
```

That last line earns its place — Codex reliably reports honest "did not fix" notes, and
they are often a real second bug worth filing.

---

## 3. Launch in the background, logging to a file

```bash
codex exec --sandbox workspace-write -C "$PWD" - \
  < /path/to/brief.md > /path/to/codex.log 2>&1
```

with `run_in_background: true`.

**Do not pipe the output through `tail`/`head`.** They buffer until the process exits, so
the log stays empty for the whole run and you are blind. Redirect to a file.

Sandbox modes: `workspace-write` is right. Never use
`--dangerously-bypass-approvals-and-sandbox`.

Use `$CLAUDE_JOB_DIR/tmp` for the brief and log when running as a background job.

---

## 4. Monitor periodically (required)

A background Bash job notifies you only at the *end*. Arm a Monitor so you get periodic
progress instead of a silent multi-minute gap:

```
Monitor({
  description: "codex progress on #NNN",
  timeout_ms: 3600000,
  persistent: false,
  command: `
    cd <root>
    # `[c]odex` prevents the monitor from matching ITSELF: pgrep -f scans full command
    # lines, and this script's own line contains the literal string "codex exec".
    while pgrep -f "[c]odex exec" >/dev/null; do
      echo "[$(date +%H:%M:%S)] files=$(git status --porcelain | wc -l) log=$(wc -l < codex.log) :: $(tail -400 codex.log | grep -Eio 'error\\[E[0-9]+\\]|test result:[^|]*|FAILED|panicked' | tail -1)"
      sleep 120
    done
    echo "[$(date +%H:%M:%S)] codex exited; changed files: $(git status --porcelain | wc -l)"
  `
})
```

Points that matter:
- **Poll ~120s.** Codex runs for many minutes; tighter polling is pure noise.
- **The loop must exit** when the process is gone, and emit a final line — otherwise the
  monitor sits armed after the work is done.
- **Silence must not look like success.** Include failure signatures (`error[E…]`,
  `FAILED`, `panicked`) in what you surface, not only progress.
- **Scan the log's tail, not the whole file.** `grep <whole log> | tail -1` reports the
  last match *ever*, so a compile error from ten minutes ago keeps being re-reported long
  after Codex fixed it and moved on. `tail -400 | grep | tail -1` reflects recent activity.
- **`files=` is the honest liveness signal**; `log=` is not. Codex dumps its full diff at
  the end, which can add thousands of lines in one burst and look like frantic activity.

Cheap manual check at any time — file churn is the best liveness signal:

```bash
git status --short && tail -c 400 codex.log
```

---

## 5. Review the diff — this is the part that matters

**A green test run from Codex is necessary, not sufficient.** In practice its self-report
has been accurate about what it *did*, and its fixtures have still been incomplete.

```bash
git diff --stat
git diff -- <the core file>
```

Read the actual diff. Then, specifically:

- **Check the seams.** Every gap found so far sat exactly at the boundary between the
  briefed task and adjacent code the brief did not mention. Codex fixes what you pointed
  at; it does not know what recently changed nearby.
- **Look for fallback branches that silently degrade.** A `_ =>` or `other =>` arm that
  copies, ignores, or wraps is where a new variant leaks through wrongly.
- **Re-run the original repro yourself**, plus the *compositions* the fix newly makes
  possible. Those cannot have been in Codex's fixtures, because they did not exist until
  its change landed.
- **Re-run the full suite and clippy yourself.** Do not take the report's word.

Worked example — delegating metel-core#282 (`&` on a field snapshotting instead of
aliasing): Codex's approach and self-report were both correct, tests were green, and two
real gaps remained. `&*r` (reborrow) re-wrapped the new value variant into a fresh cell,
producing an internal error; and `&var *r` on a shared reference failed at run time rather
than compile time. Both lived at the seam with the immediately-preceding RFC-0110 work.

---

## 6. Finish the work yourself

- **You** write the commit message, and it credits the delegation plainly
  (`The bulk of this change was delegated to a codex agent.`) along with anything you
  fixed on top.
- **You** push, close the issue, and write the closing comment.
- Never let Codex commit. `git log` attribution and message quality stay yours.
- Follow this repo's usual rules: no Claude/Codex trailers in commit messages; metel-docs
  commits go to `main` first, then bump the submodule pointer.

---

## Failure modes

| Symptom | Cause |
|---|---|
| Log file empty for minutes | Output piped through `tail`/`head`. Redirect to a file. |
| Two runs clobber each other | Concurrent `codex exec` on one worktree. Run sequentially. |
| Cannot tell what changed | Tree was dirty at launch. Commit or stash first. |
| Codex "fixed" an open design question | Brief did not name it as off-limits. Always list them. |
| Fixture added but tests nothing | Helper function never called from `main`. |
| Green tests, broken composition | Review gap — see §5. Test what the fix newly enables. |
| Codex says its build "went inert" | A release rebuild here takes minutes. Tell it so in the brief (§2), and confirm liveness yourself with `pgrep -af "cargo\|rustc"` before intervening. |
| Monitor reports a stale error forever | Filter scanned the whole log instead of its tail (§4). |
| Monitor reports "failed" ambiguously | Case-insensitive `FAILED` also matches cargo's benign `0 failed`. Check the log before reacting. |
| Monitor keeps firing after Codex exits | `pgrep -f "codex exec"` matches the monitor's own command line. Use `[c]odex exec`, and `TaskStop` the monitor when done. |
| Codex reports clippy-only verification | Its sandboxed cargo could not return exit codes. Its work is then **entirely unverified** — run the full gate yourself before believing any of it. |
