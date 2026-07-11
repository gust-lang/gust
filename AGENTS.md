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

Sprints are repository branches; issues track the work within them. Sprint branches still use the `sprint/<N>` convention.

### Starting a Sprint

1. Confirm the milestone this sprint targets, and which open issues belong to it.
2. Create the branch from current `main`:

```bash
git checkout main
git pull --recurse-submodules
git checkout -b sprint/N
git push -u origin sprint/N
```

3. Keep all sprint code, docs pointer updates, and release-prep commits on `sprint/N`.

### During a Sprint

- Read the relevant issue before editing code.
- Keep commits on the sprint branch.
- Push after each logical unit of completed work.
- If public docs changed, commit in `docs/` first, then commit the updated submodule pointer in this repo.

### Closing a Sprint

Before opening a pull request from `sprint/N` to `main`, run the quality gate below. If any gate fails, fix it on the sprint branch and run the gate again.

1. **Tests** - `cargo test` from `metel-interpreter/` must pass with zero failures.
2. **Code quality** - review every file in `git diff main..HEAD --name-only` for stale code, dead branches, accidental `todo!()`, `unimplemented!()`, `unreachable!()`, and fallible `unwrap()`/`expect()` paths.
3. **Coverage** - every feature or fix needs a focused regression test:
   - Parser or grammar changes: parsing tests or typechecking tests.
   - Type system changes: typechecking tests in `tests/typechecking/sources/`.
   - Evaluator/runtime changes: evaluator tests in `tests/evaluator/sources/` or module semantics tests.
   - Module graph/name-resolution changes: `tests/module_loading/` or `tests/module_semantics/`.
4. **Spec accuracy** - every language-visible change is documented in `docs/public/reference/spec.md` and the linked spec section.
5. **Changelog** - version-visible work is recorded in `docs/public/release-notes/changelog.md`.
6. **RFC state** - `python3 docs/internal/rfcs/tools/rfc.py check` reports clean (frontmatter matches directory, no dangling references); any RFC at `3-integrated` or beyond has `impl_status`/`impl_tracking` set correctly per `docs/internal/rfcs/PROCESS.md`.
7. **Internal docs** - update `metel-interpreter/docs/architecture.md`, `typechecker.md`, or `evaluator.md` when the corresponding pipeline, inference, construction, runtime, or builtin behavior changes.
8. **Decision records** - add a new ADR in `metel-interpreter/docs/decisions/` for non-obvious architectural decisions, reversals, or workarounds future contributors must know.
9. **Issues** - completed issues have satisfied acceptance criteria and are closed; deferred work is an explicit open issue, not hidden in a comment.

After the gate passes, open a pull request from `sprint/N` to `main` on Codeberg. The pull request diff is the authoritative sprint deliverable.

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

Types: `feat`, `fix`, `refactor`, `test`, `docs`.

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

During an active sprint, commit only on `sprint/N`, not directly on `main`.

---

## Spec Discipline

- The spec is the source of truth for language-visible behavior.
- The spec contains rules and syntax, not rationale, history, or open questions. Put rationale in RFCs or ADRs.
- New public behavior must be documented in `docs/public/reference/spec/`.
- Runtime builtins documented in `docs/public/reference/spec/runtime.md` must match what the interpreter registers.
- Version-visible changes must be reflected in `docs/public/release-notes/changelog.md`.
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
- Do not commit sprint work directly to `main`.
- Do not re-introduce a synced "RFC status" field on an issue or elsewhere — the RFC file's own directory/frontmatter is the only source of truth for RFC lifecycle state (see RFC Workflow above); this is a deliberate simplification versus how Plane was used, not an oversight.
