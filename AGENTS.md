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
| `docs/reports/` | Design reports and longer-form research notes |
| `metel-interpreter/docs/architecture.md` | Interpreter pipeline and component boundaries |
| `metel-interpreter/docs/typechecker.md` | Typechecker theory and implementation notes |
| `metel-interpreter/docs/evaluator.md` | Runtime values, signals, environment, and evaluator notes |
| `metel-interpreter/docs/decisions/` | Architectural decision records |

Public docs no longer live at `docs/public/spec.md`, `docs/public/spec/`, or `docs/public/changelog.md`. Those paths are stale.

**Known stale content, not yet fixed:** `docs/public/reference/spec.md`'s frontmatter still says `version: v0.7.0` and its Overview still describes the memory model as "reference counting, no ownership semantics required" — both predate the affine-ownership/allocator RFC cluster (RFC-0063/0065/0066/0067/0068/0071/0073/0077, accepted; RFC-0067a/0072/0078/0081/0082/0083, integrated) and need a real update pass, tracked as its own piece of work, not folded into this one.

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

**Sprints stay git branches, unchanged** — `sprint/N`, exactly as in "Sprint
Workflow" below. A sprint's issues are whichever open issues are actually being
worked against that branch; there is no separate "cycle" object to keep updated
the way Plane's cycles needed one. If grouping sprint issues visually is wanted, use
a `sprint-N` label or a Codeberg Project (kanban) board — neither is required.

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

## Sprint Workflow

Three branch tiers, in order: `sprint/N` (active work) -> `develop` (accumulates
finished sprints) -> `main` (released only). This exists so `main` always reflects
an actual release, `develop` is a real "done, not yet released" staging area
instead of the ad hoc gap that used to sit between a sprint branch and `main`, and
per-issue branches/PRs stay unnecessary for solo work — a sprint branch is already
the unit of review.

Sprints are repository branches; issues track the work within them. Sprint branches still use the `sprint/<N>` convention.

### Starting a Sprint

1. Confirm the milestone this sprint targets, and which open issues belong to it.
2. Create the branch from current `develop` (not `main` — `main` only moves at
   release time and may lag `develop` by more than one sprint):

```bash
git checkout develop
git pull --recurse-submodules
git checkout -b sprint/N
git push -u origin sprint/N
```

3. Keep all sprint code, docs pointer updates, and release-prep commits on `sprint/N`.

### During a Sprint

- Read the relevant issue before editing code.
- Keep commits on the sprint branch.
- Push after each logical unit of completed work.
- If public docs changed, commit in `docs/` first — straight to `metel-docs main`; that
  repo is trunk-based with no branch tier of its own, see its `README.md` — then
  commit the updated submodule pointer here, on `sprint/N`. The pointer is never
  bumped directly on `develop` or `main`: `develop`'s pointer only moves as a side
  effect of a sprint merging in (it's fine for it to lag `metel-docs main` between
  sprints — treat it like a dependency pin, not a freshness target), and `main`'s
  only moves at release time (see "Release Workflow" below).
- **Update `docs/public/release-notes/changelog.md` in the same commit/session that lands the feature or fix, not later.** Add the entry under the current in-progress version's section (create it, marked "in progress on `sprint/N` — not yet released", if this is the first change of the sprint targeting a new version). The sprint-close gate below re-checks completeness; it is not when the changelog is first touched.

### Closing a Sprint

Before opening a pull request from `sprint/N` to `develop`, run the quality gate below. If any gate fails, fix it on the sprint branch and run the gate again.

1. **Tests** - `cargo test` from `metel-interpreter/` must pass with zero failures.
2. **Code quality** - `cargo clippy --release --lib -- -W clippy::pedantic` from
   `metel-interpreter/` must end at **0 warnings**. The `--lib` scope is deliberate: `--all-targets`
   also lints measurement binaries and test harnesses, which are held to a looser bar. Then review
   every file in `git diff develop..HEAD --name-only` for stale code, dead branches, accidental
   `todo!()`, `unimplemented!()`, `unreachable!()`, and fallible `unwrap()`/`expect()` paths.
3. **Coverage** - every feature or fix needs a focused regression test:
   - Parser or grammar changes: parsing tests or typechecking tests.
   - Type system changes: typechecking tests in `tests/typechecking/sources/`.
   - Evaluator/runtime changes: evaluator tests in `tests/evaluator/sources/` or module semantics tests.
   - Module graph/name-resolution changes: `tests/module_loading/` or `tests/module_semantics/`.
4. **Spec accuracy** - every language-visible change is documented in `docs/public/reference/spec.md` and the linked spec section.
5. **Changelog** - confirm `docs/public/release-notes/changelog.md`'s in-progress section is actually complete against what this sprint shipped. This is a completeness check, not first authorship — see "During a Sprint" above.
6. **RFC state** - `python3 docs/internal/rfcs/tools/rfc.py check` reports clean (frontmatter matches directory, no dangling references); any RFC at `3-integrated` or beyond has `impl_status`/`impl_tracking` set correctly per `docs/internal/rfcs/PROCESS.md`.
7. **Internal docs** - update `metel-interpreter/docs/architecture.md`, `typechecker.md`, or `evaluator.md` when the corresponding pipeline, inference, construction, runtime, or builtin behavior changes.
8. **Decision records** - add a new ADR in `metel-interpreter/docs/decisions/` for non-obvious architectural decisions, reversals, or workarounds future contributors must know.
9. **Issues** - completed issues have satisfied acceptance criteria and are closed; deferred work is an explicit open issue, not hidden in a comment.

After the gate passes, open a pull request from `sprint/N` to `develop` on Codeberg (not `main` — see "Release Workflow" below for how `develop` reaches `main`). The pull request diff is the authoritative sprint deliverable.

---

## Release Workflow

A release is the `develop -> main` merge, tag, and Codeberg Release together —
distinct from, and less frequent than, a sprint merging into `develop`. `develop`
may sit ahead of `main` across several completed sprints before a release is cut;
there is no fixed cadence requirement, though in practice a release tends to line
up with a version milestone (`docs/internal/versioning.md`) reaching completion.

### Release Gate

Before merging `develop` into `main`, run this gate. It exists specifically to
catch changelog/spec drift relative to what's actually merged — the exact failure
mode this workflow is designed against — not to re-run the sprint-close gate.

1. **Changelog finalized** - `docs/public/release-notes/changelog.md`'s in-progress
   section is complete and accurate against everything merged into `develop` since
   the last release. Reword for clarity if needed, then replace the "in progress on
   `sprint/N` — not yet released" line with the release date.
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

### Cutting the Release

1. Merge `develop` into `main` (a real merge commit, not a rebase — `main`'s
   history should show exactly which sprints/PRs went into each release).
2. Tag `main` at the merge commit: `git tag vX.Y.Z && git push origin vX.Y.Z`.
3. Create a Codeberg Release from that tag, with the release body sourced from
   the changelog section just finalized (not regenerated separately — the
   changelog is the single source of truth for release notes).
4. If public documentation changed, follow "Wiki and Public Docs Release
   Workflow" below (`metel-website` pointer, versioned snapshot) as part of the
   same release, not a separate later step.

---

## Task Workflow

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

1. All acceptance criteria are satisfied.
2. Relevant tests pass; for typechecker or inference changes, the full `cargo test` suite passes.
3. Spec, changelog, RFC, internal docs, and ADR updates are complete where required.
4. Close the issue.

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

Briefs for delegated work belong in the job's scratch directory, not the repository.

---

## Adversarial Review

Before a substantial branch merges — a new analysis pass, a type-system change, anything where being
confidently wrong is worse than being incomplete — it gets an **adversarial review**: a reviewer whose
brief is to break it, not to assess it. A review that summarises the branch, or reports that it looks
good, has produced nothing. This is distinct from the sprint-close gate, which checks that the required
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
  real inflection point), not calendar-based.
- `OBJECTIVES.md` does not replace `docs/internal/rfcs/INDEX.md` (RFC-level thematic
  state) or `PROCESS.md` (RFC lifecycle mechanics) — it's the layer above both, tracking
  why priorities are what they are, not RFC-by-RFC status.

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

During an active sprint, commit only on `sprint/N`, not directly on `develop` or `main`.

---

## Spec Discipline

- The spec is the source of truth for language-visible behavior.
- The spec contains rules and syntax, not rationale, history, or open questions. Put rationale in RFCs or ADRs.
- New public behavior must be documented in `docs/public/reference/spec/`.
- Runtime builtins documented in `docs/public/reference/spec/runtime.md` must match what the interpreter registers.
- Version-visible changes must be reflected in `docs/public/release-notes/changelog.md` when the change lands, not batched for later — see "Sprint Workflow" above.
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
2. Update this repo's `docs` submodule pointer on the sprint branch.
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
- Do not commit sprint work directly to `develop` or `main`.
- Do not merge `develop` into `main` outside the Release Workflow's gate — `main` only moves at an actual release.
- Do not re-introduce a synced "RFC status" field on an issue or elsewhere — the RFC file's own directory/frontmatter is the only source of truth for RFC lifecycle state (see RFC Workflow above); this is a deliberate simplification versus how Plane was used, not an oversight.
