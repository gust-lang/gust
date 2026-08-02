# Metel - Agent Guide

## Project

Metel is a statically typed, expression-oriented language. This repository contains the tree-walk interpreter and consumes the shared documentation repository as the `docs/` submodule.

The interpreter is the shipped runtime. Treat it as the current product surface, not as throwaway compiler scaffolding. The language specification is the contract the interpreter must satisfy.

The repository remote is Codeberg (`codeberg.org/metel-lang/metel-core`). Task tracking is in **Codeberg Issues** on this repository, not GitHub Projects, and not Plane — see "Task Tracking: Codeberg Issues" below. This repo previously used Plane; that migration is complete and Plane is no longer the source of truth for anything.

---

## Current Documentation Structure

`docs/` is the shared `metel-docs` submodule. Update it as a real submodule: make docs edits in the submodule, commit them there, then update the pointer in this repo.

| Location | Purpose |
|---|---|
| `docs/README.md` | Authoritative public/internal docs layout |
| `docs/public/getting-started/` | Intro, quickstart, and tutorials |
| `docs/public/reference/spec.md` | Language specification entry point |
| `docs/public/reference/spec/` | Spec sections: lexical, types, declarations, functions, expressions, modules, runtime, grammar |
| `docs/public/reference/error-codes.md` | Error code reference |
| `docs/public/release-notes/changelog.md` | Version changelog and release notes |
| `docs/internal/versioning.md` | Version numbering and doc/changelog conventions. **Not** the RFC lifecycle — see below. |
| `docs/internal/rfcs/PROCESS.md` | **Authoritative RFC lifecycle, working rules, and tooling reference.** Read this before touching any RFC. |
| `docs/internal/rfcs/INDEX.md` | Thematic snapshot of all RFCs by cluster and status — check before opening a new RFC to avoid duplicating one that already exists. |
| `docs/internal/rfcs/tools/rfc.py` | The lifecycle tool (`new`/`transition`/`supersede`/`check`/`index`) — mechanizes the procedural parts of PROCESS.md. Run `rfc.py check` after any manual RFC edit. |
| `docs/internal/rfcs/0-draft/` | Draft RFCs being written |
| `docs/internal/rfcs/1-under-review/` | RFCs ready for evaluation |
| `docs/internal/rfcs/2-accepted/` | Design settled; not yet integrated into the spec |
| `docs/internal/rfcs/3-integrated/` | Merged into `docs/public/reference/spec/` with worked examples checked for soundness against everything else already integrated; not yet implemented — see `impl_status`/`impl_tracking` below |
| `docs/internal/rfcs/4-implemented/` | Implemented and shipped |
| `docs/internal/rfcs/5-superseded/` | RFCs replaced by later RFCs |
| `docs/internal/rfcs/6-refused/` | RFCs refused with a recorded decision |
| `docs/reports/strategy/OBJECTIVES.md` | **Living long-term objectives, current priorities, and open triggers** — persists across planning cycles; see "Strategic Planning" below. |
| `docs/reports/strategy/PROCESS.md` | **How to run a strategic-overview cycle** — verification discipline, trigger lifecycle, and the dated overview's structural template. Read before running one. |
| `docs/reports/` | Design reports and longer-form research notes |
| `metel-interpreter/docs/architecture.md` | Interpreter pipeline and component boundaries |
| `metel-interpreter/docs/typechecker.md` | Typechecker theory and implementation notes |
| `metel-interpreter/docs/evaluator.md` | Runtime values, signals, environment, and evaluator notes |
| `metel-interpreter/docs/decisions/` | Architectural decision records |

Public docs no longer live at `docs/public/spec.md`, `docs/public/spec/`, or `docs/public/changelog.md`. Those paths are stale.

*(Removed 2026-08-02, during the v0.12.0 pre-release review: this slot previously warned that `spec.md`'s frontmatter said `version: v0.7.0` and that its Overview described the memory model as "reference counting, no ownership semantics required". **Both were fixed some releases ago and the warning outlived them** — verified directly against the file. Kept as a note rather than deleted silently, because a stale warning is worse than none: it trains readers to skip warnings. `internal/versioning.md`'s release checklist now carries a standing item to re-check notes like this one each release.)*

---

## Task Tracking: Codeberg Issues

Codeberg Issues on this repository (`codeberg.org/metel-lang/metel-core`) are the source of
truth for implementation tasks, labels, and version milestones. This replaces Plane
(migrated away from for the same reason Plane replaced an earlier tool: avoid vendor
lock-in on task state that lives nowhere in the repo itself). It also replaces
ClickUp, which was used briefly between Plane and this migration but was never
written down here.

**RFC lifecycle tracking no longer needs a mirrored issue type or synced custom
property.** Plane needed a custom `RFC` work-item type plus an `RFC Status` property
kept in sync with the RFC file by hand (or by API call) because Plane had no native
notion of "this issue's status lives in a git file." Codeberg issues don't try to
mirror RFC status at all — the RFC file's own directory and frontmatter (`status`,
and from `3-integrated` onward, `impl_status`/`impl_tracking`) are already the single
source of truth, per `docs/internal/rfcs/PROCESS.md`. An RFC gets an issue only once
it reaches `3-integrated` and needs real implementation tracked — one issue per RFC
(or per tightly-coupled cluster), linked back via that RFC's `impl_tracking` field.
There is nothing to keep in sync in the other direction; the issue never needs to
restate the RFC's lifecycle stage.

**Labels** — carry over Plane's "Product modules" as labels: `interpreter`, `wiki`,
`compiler`, `playground`, `lsp`. Add labels for cross-cutting concerns as they
recur (e.g. `blocked`, `needs-design`) rather than up front — don't pre-invent a
taxonomy nothing has asked for yet.

**Milestones** — version milestones (`v0.10.0`, `v0.11.0`, ...) map directly from
Plane's version-milestone use; no change in concept, just in host.

**There is no sprint object, and no sprint branch** — see "Branch Workflow" below for
why `sprint/N` was retired. The unit of work is the issue; the unit of review is the
pull request that closes it; the unit of grouping is the **version milestone**. Nothing
needs a separate "cycle" object the way Plane's cycles did, and nothing needs a
`sprint-N` label either — the milestone already answers "what is this batch of work
for," and unlike a sprint number it answers it in a way the changelog and the release
gate both read from.

**Dependencies** between issues: reference by number in the issue body (`Blocked by
#42`, `Blocks #57`) — Gitea/Codeberg's issue references render these as links but
don't enforce blocking; treat the same as Plane's `blocked_by`/`blocking` relations
were treated, as documentation, not enforcement.

**Rate limits on creating issues/comments.** Codeberg enforces a tight anti-spam
guard on issue and comment creation — roughly 5 issue creates or ~15 comment posts
per account per 5-minute window (observed empirically, not documented; not the
general API rate limit, which is much higher). This is a nonprofit, donation- and
membership-funded instance (Codeberg e.V.) with no paid tier that lifts it. Creating
more than a handful of issues/comments in one sitting (a bulk migration, splitting a
task into several subissues, closing out a batch of stale issues) **will** hit this.
Use `tools/tea-paced.sh <tea subcommand and args>` instead of calling `tea` directly
for any such batch — it retries with backoff specifically on a rate-limit response
and fails fast (no retry) on any other error. It does not pre-emptively pace calls;
a bulk loop should still put a pause (60-90s) between individual creates.

Common actions:

- Read a task: fetch the issue by number.
- Search tasks: filter issues by label, milestone, or state (`open`/`closed`).
- Start task work: leave the issue open, reference it in commits as work proceeds.
- Finish task work: close the issue only after acceptance criteria and tests pass.
- Version planning: assign the milestone.

Do not rely on `.github/` automation or GitHub issue labels — this is a Codeberg
(Gitea/Forgejo) repository, not GitHub; there is no `.github/` directory here.

---

## Branch Workflow

Two branch tiers: **issue branches** (active work) -> **`develop`** (integration trunk)
-> **`main`** (released only). One issue, one branch, one pull request, fast-forwarded
into `develop`. `main` moves only at a release, also by fast-forward, and is tagged
there. `develop` is the "done, not yet released" staging area that makes `main` mean
"an actual release" rather than "wherever work happened to be."

Branch names are `<type>/<issue>-<slug>`, using the same types as the commit
convention: `feat/291-move-checking`, `fix/314-array-intrinsics-through-view`,
`refactor/304-structural-type-params`, `docs/…`, `chore/…`, `test/…`. Work with no
tracked issue omits the number (`chore/drop-sprint-branches`), which should be rare —
if it's worth a branch it's usually worth an issue.

### Why `sprint/N` was retired

The `sprint/N` tier existed as a third layer between issue branches and `develop`, and
was removed in v0.12.0 because measurement showed it was carrying no information. This
is recorded because it will otherwise look like drift and get "restored":

- **A sprint branch outlived a release.** `v0.11.0` is tagged at `c641938`, a commit
  *inside* `sprint/27` — 116 commits in, with 72 more landing after it. The documented
  model said a release is `develop -> main`; what happened is `main` fast-forwarded to a
  point on a sprint branch that then kept going. The tier boundary did not hold in
  practice even while it was written down.
- **The issue branch was already the real review unit.** Ten branches (`feat/288`,
  `feat/289`, `feat/290`, `feat/291`, `refactor/304`, `fix-257-271`, `fix-272`,
  `fix-274`, `chore/ff-only-merges`, `docs/codex-review-workaround-lesson`) landed
  through their own pull requests inside `sprint/27`'s range alone, while the guide
  still claimed per-issue branches were "unnecessary."
- **Sprint pull requests were omnibus diffs, not reviewable units.** #320 bundled a
  fixture migration, an RFC through four lifecycle stages, a new stdlib method, a
  stdlib-wide `self`/`&self` audit, and temporary lifetime extension.
- **The number meant nothing.** `sprint/21`–`24` ran one to three days each; `sprint/25`
  ran four weeks; `sprint/27` ran eleven days and spanned a release. It was never a
  time box, and version milestones already do the grouping it stood in for.
- **Under fast-forward-only merging, the third tier is a third name for one line of
  commits.** At retirement `develop` and `sprint/27` were not merely equivalent, they
  were the same SHA.

What the sprint tier did genuinely provide was a *periodic* gate — a moment to run
`rfc.py check`, refresh internal architecture docs, and sweep issue hygiene. That did
not disappear; it moved to the release gate, which is the boundary that still exists.
See "The Two Gates" below.

### Merging: Fast-Forward Only

**Every merge in these repositories is a fast-forward.** That holds at both tiers — an
issue branch into `develop`, and `develop` into `main` at release. History stays linear,
so `git log` on any branch reads as the
order work actually landed, and `git bisect` never has to pick a parent.

The consequence is that **a branch must be rebased onto its target before it can
merge**. If the target moved while you were working, rebase the branch onto it and
force-push *the branch* — a topic branch you own, never the target. Do not merge
the target back into the branch to catch up: that creates exactly the merge commit
this rule exists to prevent.

`tea pr merge` cannot do this. Its `--style` accepts only `merge`, `rebase`,
`squash`, and `rebase-merge`, and it defaults to `merge` — which is how PR #311
produced `5d5e561`, a merge commit for a branch that was already fast-forwardable.
Both repositories permit the style server-side, so merge a pull request through the
API instead:

```bash
curl -sS -X POST \
  -H "Authorization: token $(grep -oP '^\s*token:\s*\K\S+' ~/.config/tea/config.yml | head -1)" \
  -H 'Content-Type: application/json' -d '{"Do":"fast-forward-only"}' \
  "https://codeberg.org/api/v1/repos/metel-lang/<repo>/pulls/<index>/merge"
```

A non-fast-forwardable branch fails this call rather than silently growing a merge
commit, which is the point. Locally, use `git merge --ff-only`; never a bare
`git merge`, whose default is to create a commit when it cannot fast-forward
instead of stopping.

### Starting an Issue

1. Read the issue in full — acceptance criteria, referenced issue numbers, labels,
   milestone — and check that its dependencies are actually satisfied, not just closed.
   See "Task Workflow" below for what to read before editing code.
2. Branch from current `develop` (never from `main`, which lags by design, and never
   from another issue branch unless the dependency is genuine and stated in the PR):

```bash
git checkout develop
git pull --recurse-submodules
git checkout -b feat/<issue>-<slug>
git push -u origin feat/<issue>-<slug>
```

3. Keep every commit for that issue on that branch — code, docs pointer bumps, tests.

### While the Branch Is Open

- Push after each logical unit of completed work, not once at the end.
- **Rebase onto `develop`, never merge `develop` in.** If `develop` moved, rebase and
  force-push *your* branch. See "Merging: Fast-Forward Only" above — this is what makes
  the fast-forward possible at all.
- If public docs changed, commit in `docs/` first — straight to `metel-docs main`; that
  repo is trunk-based with no branch tier of its own, see its `README.md` — then commit
  the updated submodule pointer here, on the issue branch. The pointer is never bumped
  directly on `develop` or `main`: `develop`'s pointer only moves as a side effect of a
  branch merging in (it's fine for it to lag `metel-docs main` — treat it like a
  dependency pin, not a freshness target), and `main`'s only moves at release time (see
  "Release Workflow" below).
- **Update `docs/public/release-notes/changelog.md` as each feature or fix lands, not later.**
  Add the entry under the current in-progress version's section (create it, marked "in progress
  on `develop` — not yet released", if this is the first change targeting a new version).

  **The changelog is in the `docs/` submodule, so this can never be one commit** — the code
  commit lands here, the entry lands in `metel-docs`, and the pointer bump lands here again.
  That split is why this rule quietly failed for the whole of v0.12.0: an instruction to write
  the entry "in the same commit" describes something impossible, so it degraded into "later",
  which meant never. Treat it as a **pair**: the code commit, then immediately the docs commit
  plus pointer bump. Do not batch entries at the end — that is reconstruction from `git log`,
  and it loses exactly the reasoning that makes an entry worth reading.

  **Check it with `tools/changelog-status.sh`.** It lists every unreleased `feat`/`fix` commit
  that touched real code and flags the ones landing after the changelog was last edited, so the
  gate below is a diff to read rather than something to remember. It reports rather than
  enforces — timestamps are a heuristic, since entries are prose and don't cite issue numbers.

### The Two Gates

Quality checks split by how often they can meaningfully change. **Per-PR** checks are
cheap, scoped to one diff, and must run every time — a green one means the branch is
safe to fast-forward. **Release** checks sweep repository-wide state that no single diff
owns; running them per-PR would be noise, since the answer barely moves between issues.
They live in "Release Gate" below.

This split is what replaced the single sprint-close gate. Nothing was dropped: items 1–5
of that gate became per-PR (they were always per-diff questions wearing a sprint-sized
label), and 6–9 became release-gate items (they were always periodic sweeps).

### Closing an Issue

Run the per-PR gate before opening the pull request. If any check fails, fix it on the
branch and run again.

1. **Tests** - `cargo test --release` from `metel-interpreter/` must pass with zero failures.
   Confirm by reading the `test result:` lines, not by the command exiting — a wrapper shell
   exiting has been mistaken for a finished test run more than once.
2. **Formatting** - `cargo fmt --check` from `metel-interpreter/` must pass with no diff.
   Run `cargo fmt` before committing whenever it reports drift. Formatting is repository-wide:
   do not hand-format only the touched hunks or leave pre-existing drift for the next branch.
3. **Code quality** - `cargo clippy --release --lib -- -W clippy::pedantic` from
   `metel-interpreter/` must end at **0 warnings**. The `--lib` scope is deliberate: `--all-targets`
   also lints measurement binaries and test harnesses, which are held to a looser bar. Then review
   every file in `git diff develop..HEAD --name-only` for stale code, dead branches, accidental
   `todo!()`, `unimplemented!()`, `unreachable!()`, and fallible `unwrap()`/`expect()` paths.
4. **Coverage** - every feature or fix needs a focused regression test. Fixtures live under
   `metel-interpreter/tests/integration/sources/`:
   - Parser or grammar changes: `parsing/`, or a typechecking fixture.
   - Type system changes: `typechecking/`.
   - Evaluator/runtime changes: `evaluator/`.
   - Module graph/name-resolution changes: `module_loading/` or `module_semantics/`.

   A directory fixture's sidecar is `test.toml`; a single-file fixture's is `<name>.toml`.
   Getting this wrong makes the sidecar silently inert — the fixture passes while checking
   nothing, which is how six migrated fixtures skipped move checking entirely.
5. **Spec accuracy** - every language-visible change is documented in `docs/public/reference/spec.md` and the linked spec section.
6. **Changelog** - `tools/changelog-status.sh` reports no unlogged commits for this branch's
   work. This is a completeness check, not first authorship — see "While the Branch Is Open"
   above. A failure here means entries were batched, not that the check is noisy.
7. **Acceptance criteria** - every criterion in the issue is actually satisfied. Deferred
   work is a new open issue, not an unchecked box or a comment.

Then open a pull request to `develop` (never to `main` — see "Release Workflow" below for
how `develop` reaches `main`). **One issue per pull request.** If the branch grew a second
concern along the way, that concern is its own issue and its own branch; the fact that it
was discovered here is not a reason to ship it here. Sprint-era omnibus pull requests are
exactly what this rule exists to prevent.

Merge by fast-forward, then close the issue. Delete the branch once merged — an issue
branch has no life after its pull request, and leaving them accumulates the same
uncertainty about "is this merged?" that sprint branches did.

---

## Release Workflow

A release is the `develop -> main` fast-forward, tag, and Codeberg Release together —
distinct from, and much less frequent than, an issue branch merging into `develop`.
`develop` sits ahead of `main` across many closed issues before a release is cut. **A
release is cut when a version milestone completes**, not on a calendar — that is the
only cadence rule, and it is why the milestone rather than a sprint number is the unit
of grouping (see "Task Tracking" above).

### Release Gate

Before merging `develop` into `main`, run this gate. It is the periodic half of "The
Two Gates" above: it catches changelog/spec drift relative to what's actually merged —
the exact failure mode this workflow is designed against — plus the repository-wide
sweeps that no single pull request owns. It does **not** re-run the per-PR gate; those
checks already passed on every branch that landed.

1. **Changelog finalized** - `docs/public/release-notes/changelog.md`'s in-progress
   section is complete and accurate against everything merged into `develop` since
   the last release. Reword for clarity if needed, then replace the "in progress on
   `develop` — not yet released" line with the release date.
2. **Version number chosen** - per `docs/internal/versioning.md`'s major/minor/patch
   rule (spec changes require at least a minor bump; a patch must not touch
   language-visible behavior at all). Bump `metel-interpreter/Cargo.toml`'s
   `version` to match, in the same commit as the changelog finalization.
3. **RFC state** - `python3 docs/internal/rfcs/tools/rfc.py check` reports clean.
   Any RFC the release actually implements end-to-end should be at `4-implemented`
   (`rfc.py transition <id> --to implemented`), not left at `3-integrated` with
   stale "Not yet implemented" spec callouts.
4. **Docs submodule in lockstep** - bump the `docs` submodule pointer to
   `metel-docs main`'s current tip as part of the release commit (this is the
   only time `main`'s pointer moves at all). Since `metel-docs` is trunk-based
   with no long-lived branches of its own, its `main` should already hold
   everything the changelog/spec entries above were written against — this step
   should never require reconciling an unmerged docs branch first.
5. **Spec correctness** - spot-check that `docs/public/reference/spec.md` and its
   linked sections actually describe the behavior being released, not a stale or
   aspirational version of it.
6. **Internal docs** - `metel-interpreter/docs/architecture.md`, `typechecker.md`, and
   `evaluator.md` reflect the pipeline, inference, construction, runtime, and builtin
   behavior as it now is. Read them against the release's diff rather than from memory.
7. **Decision records** - every non-obvious architectural decision, reversal, or
   workaround this release introduced has an ADR in `metel-interpreter/docs/decisions/`.
   A reversal especially: the reason a past decision stopped holding is the part that
   gets lost, and the part the next person needs.
8. **Issue hygiene** - the milestone's issues are genuinely closed with acceptance
   criteria satisfied, and anything deferred out of the release is an explicit open
   issue re-milestoned to a later version, not left silently attached to this one.

### Cutting the Release

1. Fast-forward `main` to `develop` (`git merge --ff-only develop`, per "Merging:
   Fast-Forward Only" above). There is no release merge commit: because every issue
   branch fast-forwarded into `develop`, `main` is by construction a prefix of it,
   and what went into a release is read off the tags bracketing the range, not off
   a merge node.
   **Freeze `develop` for the duration.** `main` must land on `develop`'s tip, not on
   a commit somewhere inside a longer line of work — `v0.11.0` was tagged 116 commits
   into `sprint/27` with 72 more still to come, which is how a release ended up
   bracketing a range nobody had decided on. Merge nothing into `develop` between the
   gate passing and the tag being pushed.
2. Tag `main` at its new tip: `git tag vX.Y.Z && git push origin vX.Y.Z`.
3. Create a Codeberg Release from that tag, with the release body sourced from
   the changelog section just finalized (not regenerated separately — the
   changelog is the single source of truth for release notes).
4. If public documentation changed, follow "Wiki and Public Docs Release
   Workflow" below (`metel-website` pointer, versioned snapshot) as part of the
   same release, not a separate later step.

---

## Task Workflow

"Branch Workflow" above covers the mechanics — where commits go, how a pull request
lands. This covers the substance: what to read before writing code, and what "done"
means beyond the checks passing.

### Before Starting a Task

1. Retrieve and read the full issue, including acceptance criteria, dependencies (referenced issue numbers), labels, and milestone.
2. Read every spec section the task touches. The spec entry point is `docs/public/reference/spec.md`.
3. Read relevant RFCs in `docs/internal/rfcs/` and ADRs in `metel-interpreter/docs/decisions/`.
4. Check dependency issues and confirm their implementation matches the contract this task depends on.
5. If the spec is missing or ambiguous, update the spec first. If the choice is non-obvious, write an ADR before implementation.
6. Note in the issue (a comment, or just the first commit) that work has started.

### During Implementation

- Follow the spec exactly. If behavior is not in the spec, it does not exist yet.
- Do not implement undocumented behavior and plan to fix docs later.
- Keep scope tight. If required work falls outside the task, open or update an issue and only proceed if it is a real blocker.
- Preserve user changes in the worktree. Never revert unrelated dirty files.
- Keep docs submodule changes and root-repo pointer changes distinct.

### Before Marking Done

Run the per-PR gate in "Closing an Issue" above — it is the authoritative list, and is
not restated here so the two cannot drift apart. Beyond it:

- For typechecker or inference changes, the full `cargo test` suite passes, not just the
  targeted tests. Blast radius in that part of the pipeline is routinely wider than the
  change looks.
- Close the issue only after the pull request has merged, not when the code is written.

---

## Delegated Implementation Work

Implementation is sometimes handed to a sandboxed coding agent (`codex exec --sandbox
workspace-write`). That sandbox has **no network**, **read-only git metadata** (so it cannot commit —
it leaves work in the tree), and cannot reliably run `cargo` when another build holds the target lock.

**A delegated result is unverified until you have run the verification commands yourself.** This is
not a courtesy check. Reports arriving from a sandbox have twice been confidently wrong in ways the
report did not flag: a measurement number inflated by a bug in the measuring code, and a "fixed"
structural change that left four unit tests failing. An agent that says *"I could not verify this"* is
being honest, and that sentence is the normal case, not an admission of failure.

So:

- Run `cargo test --release` and the clippy gate on the returned tree before reporting anything about
  it, and before building further work on top of it.
- Treat a returned test count, sweep total, or histogram as a claim about the code, not a measurement,
  until it is reproduced.
- The same rule applies to **adversarial review findings**: reproduce the repro before fixing it, and
  once a finding is confirmed, look for siblings of the same class. Both times a review finding was
  fixed properly on this project, the fix surfaced further instances the review had not found.
- **Verify before filing an issue.** An issue filed on an unchecked premise costs more to retract than
  it did to write — check the claim against the working tree first.
- **An odd construct in the diff is a question, not a detail.** A sandboxed agent reaches for whatever
  makes the checker pass — an explicit deref, a rewritten algorithm, a hardcoded literal standing in
  for a computed read. Stop and ask why it was needed before moving past it: the answer is sometimes a
  real bug worth filing (metel-core#314), sometimes a silently dropped feature (a fixture's own header
  named "recursive functions" as covered, and the delegated fix had quietly rewritten the recursion
  away to dodge an unrelated check). A green suite does not surface either — reading the diff does. See
  `.claude/skills/codex-delegate/SKILL.md` §5 for the worked examples.

Briefs for delegated work belong in the job's scratch directory, not the repository.

---

## Adversarial Review

Before a substantial branch merges — a new analysis pass, a type-system change, anything where being
confidently wrong is worse than being incomplete — it gets an **adversarial review**: a reviewer whose
brief is to break it, not to assess it. A review that summarises the branch, or reports that it looks
good, has produced nothing. This is distinct from the per-PR gate, which checks that the required
artefacts exist; this checks whether the code is *right*.

### Briefing one

The brief does the work. A vague brief produces a summary; these are the parts that reliably produce
defects instead.

- **State the job as breaking it**, and **forbid fixing**. Findings only, experiments reverted,
  `git diff --stat` showing nothing but the branch's own changes at the end. A review that also edits
  produces a diff nobody can verify.
- **Tabulate the claims** — every behaviour the branch asserts, and the file that implements it. The
  reviewer checks claims against code, rather than forming an impression of it.
- **Supply the failure history, specifically.** *"Stage 1 produced three separate false-positive
  classes, all found by hand-auditing real fixtures rather than by the tests. Assume there is a
  fourth."* This is the highest-yield sentence in a review brief: it aims effort at where this code has
  actually been wrong before, which is a much better prior than where a reader would look unprompted.
- **Name the decisions to attack, and say they are not settled.** *"Do not treat these as settled
  because I wrote them down."* Absent that, a reviewer reads the author's stated rationale as the spec
  and reviews the implementation against it — which cannot find a wrong decision.
- **Point at the more serious failure class.** Here it was false negatives, because the measurement
  sweep already surfaces false positives; nobody had gone looking for code that *should* be rejected
  and is not. Say which direction is under-explored and why.
- **Demand runnable repros over arguments from reading code**, and give the entry point to run them
  (the built binary, the sweep tool, the opt-in flag). An argument from reading is a hypothesis.
- **Require a severity per finding**, and require it to separate **wrong behaviour** from **poor
  diagnostic** from **untested but correct**. These get very different responses and a flat list
  obscures which is which.
- **Say plainly that an honest all-clear is an acceptable result** — while adding that "clean" should be
  a conclusion the reviewer had to work for. Without the first half, a reviewer under implicit pressure
  to justify the exercise invents findings; without the second, "looks fine" costs nothing to write.
- State the sandbox constraints up front (see above), and have the report written **outside** the
  repository.

### Consuming one

- **Reproduce a finding before fixing it.** Some will be real, some will be misreadings of code that is
  correct, and the brief's own framing ("assume there is a fourth defect") actively encourages the
  second. Reproduce first, then fix.
- **A confirmed finding is a lead, not a bug.** Ask what class it belongs to and sweep for siblings.
  Every time this was done properly here, the sweep found more instances than the review had: two
  reported symptoms of one cause, whose correct fix exposed three further defects of the same shape.
- **Prefer fixing the class over fixing the findings.** If two reported defects share a cause, the fix
  that removes the cause is worth the larger diff — a rule that holds only for the spellings someone
  happened to test is not implemented.
- **Hand the fix out as its own brief, not as a follow-up to the review.** A fix brief can carry the
  structural constraint a review brief cannot: *"do not add the two guards to the second path — that
  leaves the class intact; express the rule over the shared data structure so both paths get it by
  construction."* Then verify it yourself, per the section above.

---

## Landing an Enforcement Pass Over an Existing Corpus

A new static check (move checking, borrow checking, negative-bound enforcement, exhaustiveness) will
reject code the existing fixture corpus is full of. The corpus is not wrong; it predates the rule.

The established pattern, used for both the `Copy`/`Drop` aspects and move checking:

1. **Ship the check off by default**, behind a `RunOptions` field and a per-fixture sidecar opt-in
   (`[options] move_check = true`), wired into *every* pipeline entry point.
2. **Write new fixtures that opt in**, so the check and its diagnostics get real end-to-end coverage
   from the first commit.
3. **Do not edit an existing fixture to make a new check pass.** Many are behavioural evaluator tests
   whose meaning changes when rewritten. An existing fixture that changes behaviour under the new
   check is a finding to report and count, not a file to fix in passing.
4. **Corpus migration and flipping the default are their own issue**, filed with the measured scope
   (fixture count and violation count) so the decision to migrate is made against a number.
5. **Measure before migrating**, and make the measurement tool assert its own invariants. Three
   separate false-positive classes in one analysis were found by hand-auditing a sweep against real
   fixtures, none by the analysis's own unit tests; the durable fix is for the sweep to fail loudly on
   a self-contradictory result rather than to trust a later hand-audit.

**The limit on this pattern:** off-by-default is acceptable only while the capability is *visibly*
inert. A feature that appears to work and silently does nothing must be gated behind a rejection
before release rather than shipped — see RFC-0071 §9c, which records this for `Drop` without
destructor invocation and notes the project has already hit that failure mode twice.

---

## RFC Workflow

RFCs live in `docs/internal/rfcs/`. **`docs/internal/rfcs/PROCESS.md` is the sole
authority on the RFC lifecycle, working rules, and tooling** — read it, don't rely on
the summary below for anything beyond a quick orientation. `docs/internal/versioning.md`
covers version numbering and changelog conventions only; it does **not** define the RFC
lifecycle (its own RFC-lifecycle section is stale and superseded by PROCESS.md — see the
note in that file).

An RFC has exactly one state, represented by its directory:

- `0-draft/` - `draft`
- `1-under-review/` - `under-review`
- `2-accepted/` - `accepted`
- `3-integrated/` - `integrated` — merged into `docs/public/reference/spec/`, worked
  examples checked against everything else already integrated; not yet implemented
- `4-implemented/` - `implemented`
- `5-superseded/` - `superseded`
- `6-refused/` - `refused`

Rules:

- The RFC document is the source of truth for design details.
- The directory is the source of truth for the RFC's lifecycle state; frontmatter
  `status` must match it. Run `python3 docs/internal/rfcs/tools/rfc.py check` after any
  manual edit — it validates this and more (dangling references, duplicate ids).
- Accepted RFCs must reach `3-integrated` (spec updated, worked examples written) before
  implementation work begins — this is what used to be tracked by the now-retired
  `spec_status: pending/done` field; `3-integrated` is the actual lifecycle stage that
  replaced it, not a parallel field to also keep in sync.
- From `3-integrated` onward, the RFC's frontmatter carries `impl_status`
  (`not-started`/`in-progress`/`implemented`) and `impl_tracking` (the Codeberg issue
  link). `rfc.py transition <id> --to integrated` refuses to run without
  `--tracking <issue-url>` — no RFC enters integrated without one.
- Implementation issues should reference the RFC file they implement in the issue body.
- When implementation lands, run `rfc.py transition <id> --to implemented` — this also
  sets `impl_status: implemented`.

If an existing RFC's folder, frontmatter status, or `impl_status` contradicts
`docs/internal/rfcs/PROCESS.md`, stop and resolve the documentation workflow
inconsistency before implementing against it.

---

## Strategic Planning

Long-term objectives, current priorities, and open triggers (watch-list items that
should prompt a re-check when conditions change) live in
`docs/reports/strategy/OBJECTIVES.md` — a living document, updated in place, not a
dated snapshot. Periodic dated narrative snapshots (`docs/reports/strategy/
strategic-overview-YYYY-MM-DD.md`) remain the point-in-time record of what was found
and decided each planning cycle; `OBJECTIVES.md` is what each cycle reads from and
writes back to, so priorities and triggers persist between cycles instead of being
reconstructed from whichever dated file is most recent.

Rules (content lives in `OBJECTIVES.md` itself, not duplicated here):

- Before starting non-trivial design or planning work, check `OBJECTIVES.md`'s current
  priorities (§2) and open triggers (§3) for relevance.
- A strategic-overview cycle checks triggers against real progress, updates priorities
  in place, adds anything new, and appends to the review log — *then* decides whether a
  new dated snapshot is warranted. Triggering a new dated snapshot is event-based (a
  real inflection point), not calendar-based, and stays human-prompted rather than
  agent-initiated — see `docs/reports/strategy/PROCESS.md` §5.
- `OBJECTIVES.md` does not replace `docs/internal/rfcs/INDEX.md` (RFC-level thematic
  state) or `PROCESS.md` (RFC lifecycle mechanics) — it's the layer above both, tracking
  why priorities are what they are, not RFC-by-RFC status.
- **`docs/reports/strategy/PROCESS.md` is the methodology reference — read it before
  running a strategic-overview cycle.** It covers what's not obvious from `OBJECTIVES.md`
  alone: the verification discipline (every claim checked against a primary source, not
  restated from memory or frontmatter), the trigger append-only lifecycle and closure
  bar, and the dated overview's structural template.
- **If the user makes an explicit priority call or redirect at any point — not only
  during a strategic-overview cycle — log it immediately to `OBJECTIVES.md` §0's
  Operator Directives**, rather than waiting for the next cycle to reconstruct it after
  the fact. This is the one place operator intent enters the process as a first-class
  input rather than inferred evidence; see `docs/reports/strategy/PROCESS.md` §1.

---

## Commit Convention

Every commit related to a tracked issue should reference the issue number:

```text
<type>(#<number>): <description>
```

Types: `feat`, `fix`, `refactor`, `test`, `docs`, `chore`.

Examples:

```text
feat(#57): enforce function aspect bounds
docs(#58): update aspect bound spec text
test(#60): cover generic bound regressions
```

Commits not tied to a tracked item may omit the reference, for example `docs: point CLAUDE.md to AGENTS.md`.

When a commit is intended to close work after merge, include a body describing what changed and reference the issue — Codeberg closes an issue automatically on merge to `main` when the body contains `Closes #<number>` (or `Fixes`/`Resolves`):

```text
feat(#57): enforce function aspect bounds

- Check call-site type arguments against declared bounds
- Seed bound methods during function body inference
- Add stage12 typechecking regressions

Closes #57
```

Commit only on an issue branch, never directly on `develop` or `main`.

---

## Spec Discipline

- The spec is the source of truth for language-visible behavior.
- The spec contains rules and syntax, not rationale, history, or open questions. Put rationale in RFCs or ADRs.
- New public behavior must be documented in `docs/public/reference/spec/`.
- Runtime builtins documented in `docs/public/reference/spec/runtime.md` must match what the interpreter registers.
- Version-visible changes must be reflected in `docs/public/release-notes/changelog.md` when the change lands, not batched for later — see "Branch Workflow" above.
- Patch releases must not introduce spec changes; see `docs/internal/versioning.md`.

---

## Interpreter Architecture Invariants

The current interpreter pipeline is:

```text
.mln root file
  -> Module Loader
  -> Name Resolver
  -> Path Normalizer
  -> Type Checker
  -> Evaluator
```

Do not skip stages.

Important module-system invariants:

- `module_loader::load_root` produces a `ModuleGraph` in topological order.
- `name_resolver::resolve` owns import scopes, visibility, public surfaces, and re-exports.
- `path_normalizer::normalize` rewrites qualified paths before typechecking.
- `typechecker::check_graph` consumes the normalized graph plus resolved names and returns `TypedModuleGraph`.
- `evaluator::evaluate_graph` consumes `TypedModuleGraph`.
- Cross-module public APIs must be fully annotated; do not introduce cross-module type inference casually.

If a change alters these boundaries, update `metel-interpreter/docs/architecture.md` and consider an ADR.

---

## Type System Stability

The sensitive areas are `metel-interpreter/src/typeinference/` and `metel-interpreter/src/typechecker/`. Bugs here can produce silent wrong typing, not just crashes.

### Two-Pass Typechecker Boundary

The typechecker remains split into inference and construction:

- Pass 1 (`src/typechecker/inference.rs`): walk the AST, emit constraints, solve into substitutions, update inference context.
- Pass 2 (`src/typechecker/construction.rs`): read solved substitutions and build typed AST nodes.

Do not infer types in Pass 2. Do not build typed AST nodes in Pass 1. If a task seems to require that, stop and ask.

### Key Invariants

- `Substitution::compose` is ordered. Verify composition direction every time it is used.
- `Never` is a bottom type. Typechecking tests alone may not distinguish a diverging expression from a correctly typed runtime path; use evaluator tests for runtime behavior.
- Route conversions through `type_to_infer` where `Perhaps`/`Result` normalization matters.
- Distinguish formal `TypeVar`s from fresh `InferType::Var(TypeVar)` usage-site variables.
- **`crate::types::Type` contains no type variables** — by the time it exists, generics are
  monomorphised. An abstract type parameter is therefore `InferType::Var(TypeVar)`, and any query
  that must answer "does this abstract parameter satisfy an aspect?" belongs on the `InferType` side
  with the concrete-`Type` form as a wrapper over it. Do not add a parameter variant to `Type`, and
  do not smuggle one into a `Type::Named` name string: encoding a parameter as a mangled nominal name
  was tried and rejected, because a generic parameter must be *structurally* distinct from a nominal
  type, not distinguished by a naming convention every consumer has to re-learn.
- Generic instantiation should follow the established `instantiate_scheme_for_call` pattern: fresh variables, initial substitution, unification against actuals, then extraction from the composed substitution.
- Imported schemes must seed both inference and construction paths for a module. If only one pass sees imports, the typechecker is wrong.
- Public module declarations that are consumed cross-module must have enough annotations to export concrete schemes.

### Before Finalizing Type System Changes

1. Run `cargo test` from `metel-interpreter/`.
2. Run or manually apply the `/review-typechecker` checklist.
3. For every new `unify` call, verify expected-vs-actual argument order and substitution composition direction.
4. For every `infer_type_to_type` call, verify all type variables are resolved and a useful span is available.
5. If `construct_block` expected-type threading changes, check every call site.
6. Add regression tests that would fail without the fix.

Stop and ask if:

- You need to touch inference and construction in a way that blurs their boundary.
- No existing pattern covers the new type-system behavior.
- A substitution-order change breaks an existing test.
- The task depends on a spec interpretation that is unclear.

---

## Decision Records

Create an ADR in `metel-interpreter/docs/decisions/` when:

- Multiple reasonable implementation options exist and the chosen tradeoff matters.
- The decision changes or reverses a previous ADR or RFC.
- A workaround or limitation would surprise a future contributor.
- A spec or architecture doc changes because implementation revealed a real constraint.

Do not create ADRs for routine implementation details that follow directly from the spec.

Accepted ADRs are not edited to reverse them. Add a new ADR that supersedes the old one.

When code intentionally encodes an ADR-backed invariant that may look wrong, add a concise comment with the reason and ADR number.

---

## Wiki and Public Docs Release Workflow

The public website consumes the same `metel-docs` content through the docs submodule.

When a task or release affects public documentation:

1. Update and commit `docs/` first.
2. Update this repo's `docs` submodule pointer on the issue branch.
3. Update `metel-website` to point at the same docs commit.
4. For public releases, generate the versioned website snapshot if the release process requires it.
5. Publish only after the docs version and website pointer match.

Do not assume automatic publication unless the release workflow explicitly says it exists.

---

## When to Stop and Ask

Stop before proceeding when:

- A design decision has multiple plausible options with architectural consequences.
- The spec is ambiguous in a way that affects implementation.
- The task description contradicts current code, docs, or issue state.
- A dependency is incomplete or wrong.
- Completing the task requires a scope expansion that could affect other work.
- You are about to make an irreversible or hard-to-reverse change.

When stopping, explain what you found, the options, and the recommended path.

---

## What Not to Do

- Do not implement behavior that is not in the spec.
- Do not let implementation and docs diverge.
- Do not add rationale or history to the spec.
- Do not use GitHub Projects or `.github/` workflows as the current process — this is a Codeberg repo, GitHub tooling doesn't apply.
- Do not create new tracking documents for open work; use Codeberg Issues.
- Do not close an issue with unchecked acceptance criteria.
- Do not commit directly to `develop` or `main`. Work reaches `develop` only by fast-forwarding an issue branch that passed the per-PR gate.
- Do not put two issues in one pull request. If a second concern appeared mid-branch, it gets its own issue and its own branch.
- Do not create a `sprint/N` branch, or reintroduce a sprint tier under another name — see "Why `sprint/N` was retired"; it was removed against measurement, not taste.
- Do not merge `develop` into `main` outside the Release Workflow's gate — `main` only moves at an actual release.
- Do not create a merge commit anywhere, at any tier. Rebase the branch and fast-forward — see "Merging: Fast-Forward Only". In particular, do not reach for `tea pr merge`, whose default style is exactly the thing this forbids.
- Do not re-introduce a synced "RFC status" field on an issue or elsewhere — the RFC file's own directory/frontmatter is the only source of truth for RFC lifecycle state (see RFC Workflow above); this is a deliberate simplification versus how Plane was used, not an oversight.
