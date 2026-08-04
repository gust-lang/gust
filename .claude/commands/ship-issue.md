# /ship-issue

Finish an issue: run the per-PR gate, open the pull request against `develop`,
fast-forward it, and close the issue.

**Arguments:** `$ARGUMENTS` — issue number, e.g. `314`

**This command does not open the pull request until every gate item passes.** The gate is
defined in AGENTS.md § Branch Workflow ("Closing an Issue") — this command runs it, it
does not redefine it. If the two ever disagree, AGENTS.md wins.

---

## Step 1 — Re-read the issue

```bash
gh issue view $ARGUMENTS
```

Walk each acceptance criterion and confirm it is actually met. If any is unmet, stop and
report what remains. Deferred work becomes a **new open issue**, never an unchecked box.

## Step 2 — Per-PR gate

Run in order. If any item fails, fix it on the branch and re-run.

**1. Tests** — from `metel-interpreter/`:

```bash
cargo test --release
```

Zero failures. Confirm by reading the `test result:` lines, not by the command exiting.
For typechecker or inference changes the full suite is required, not a filtered run —
blast radius there is routinely wider than the diff looks.

**2. Code quality** — from `metel-interpreter/`:

```bash
cargo clippy --release --lib -- -W clippy::pedantic
```

Must end at **0 warnings**. `--lib` is deliberate: `--all-targets` also lints measurement
binaries and test harnesses, held to a looser bar.

Then read every file in `git diff develop..HEAD --name-only` for stale code, dead
branches, accidental `todo!()`/`unimplemented!()`/`unreachable!()`, and fallible
`unwrap()`/`expect()`. An intentional `todo!()` needs a tracking issue linked in a
comment.

Builtins must be registered in **all** required places — `src/typechecker/registry.rs`
(inference) *and* `src/typechecker/construction.rs` (construction). One missing from
construction typechecks and then fails at runtime with "undefined name".

**3. Coverage** — every feature or fix has a focused regression test:

| Change type | Test location |
|---|---|
| Parser or grammar | `tests/integration/sources/parsing/`, or typechecking fixtures |
| Type system | `tests/integration/sources/typechecking/` |
| Evaluator / runtime | `tests/integration/sources/evaluator/` |
| Module graph / name resolution | `tests/integration/sources/module_loading/`, `.../module_semantics/` |
| New error code | a negative fixture that triggers it |
| Bug fix | a regression test that would have caught the original |

A directory fixture's sidecar is `test.toml`; a single-file fixture's is `<name>.toml`.
Getting this wrong makes the sidecar silently inert — the fixture passes while checking
nothing.

**4. Spec accuracy** — every language-visible change is in `docs/public/reference/spec.md`
and the linked section. Behaviour not in the spec does not exist.

**5. Changelog** —

```bash
tools/changelog-status.sh
```

No unlogged commits for this branch's work. This is a completeness check, not first
authorship: entries should already be there from when each change landed. A failure means
they were batched.

## Step 3 — Commit and push

Stage only files relevant to the issue — never `git add -A`. Message:

```
type(#$ARGUMENTS): <short description>

- <what was done>
- <what was done>

Closes #$ARGUMENTS
```

`type` ∈ `feat`, `fix`, `refactor`, `test`, `docs`, `chore`. The body is a bullet list of
what was done, not a paraphrase of the title.

Docs changes are always a **pair**: commit in the `docs/` submodule first (straight to
`metel-docs main`), then the pointer bump here. Verify the submodule is on `main` and not
detached before committing there:

```bash
git -C docs status -sb
```

## Step 4 — Rebase, then open the pull request

```bash
git fetch origin
git rebase origin/develop
git push --force-with-lease
```

Rebase, never merge `develop` in — that creates the merge commit fast-forward-only
merging exists to prevent. Then open the pull request **to `develop`**, never to `main`.

For anything substantial — a new analysis pass, a type-system change, anything where being
confidently wrong is worse than being incomplete — get an **adversarial review** first
(AGENTS.md § Adversarial Review).

## Step 5 — Merge by fast-forward

GitHub hosted merges do not provide a true fast-forward. After review, fast-forward locally and push the target branch:

```bash
git fetch origin <target> <branch>
git switch <target>
git merge --ff-only origin/<branch>
git push origin <target>
```

A non-fast-forwardable branch fails this call instead of silently growing a merge commit.

## Step 6 — Clean up

- Confirm the issue closed (`Closes #N` should have done it); close it manually otherwise.
- Delete the merged branch, local and remote.
- If this was the last open issue in its milestone, prompt the user to run `/cut-release`.

## Notes

- If the issue resolved an RFC open question, record the resolution in the RFC and run
  `python3 docs/internal/rfcs/tools/rfc.py check`.
- If implementation of an RFC completed here, `rfc.py transition <id> --to implemented`.
- GitHub rate limits issue/comment creation (~5 creates or ~15 comments per 5 minutes).
  For batches, use `tools/tea-paced.sh`.
- Run `tea` from the repository root, not from `docs/` — that is a different repo and
  returns `IsErrIssueNotExist`.
