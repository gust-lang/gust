# /cut-release

Cut a release: run the release gate, fast-forward `main` to `develop`, tag, and publish.

**Arguments:** `$ARGUMENTS` — the version, e.g. `v0.12.0`

**Nothing is pushed until every gate item passes.** The gate is defined in AGENTS.md
§ Release Workflow — this command runs it, it does not redefine it. If the two disagree,
AGENTS.md wins.

A release is cut when a **version milestone completes**, not on a calendar. This is the
periodic half of the two gates: it sweeps repository-wide state that no single pull
request owns. It does **not** re-run the per-PR gate — those checks already passed on
every branch that landed.

---

## Step 0 — Freeze `develop`

Merge nothing into `develop` between the gate passing and the tag being pushed. `main`
must land on `develop`'s **tip**, not on a commit inside a longer line of work — that is
how `v0.11.0` ended up tagged 116 commits into `sprint/27` with 72 more still to come,
bracketing a range nobody had decided on.

Announce the freeze, then:

```bash
git checkout develop && git pull --recurse-submodules
git log --oneline $(git describe --tags --abbrev=0 origin/main)..develop
```

That range is what this release ships. Read it — it is the input to every gate item below.

## Step 1 — Milestone is actually complete

```bash
tea issues ls --milestones $ARGUMENTS --state open
```

Every remaining open issue is either genuinely done and needs closing, or is deferred and
must be **re-milestoned to a later version** — not left silently attached to this one.
Confirm closed issues had their acceptance criteria satisfied; a closed box is not proof.

## Step 2 — Release gate

**1. Changelog finalized** — `docs/public/release-notes/changelog.md`'s in-progress
section is complete and accurate against the commit range from Step 0. Reword for
clarity, then replace the "in progress on `develop` — not yet released" line with the
release date. Cross-check with:

```bash
tools/changelog-status.sh
```

**2. Version number chosen** — per `docs/internal/versioning.md`'s major/minor/patch rule:
a spec change requires at least a minor bump; a patch must not touch language-visible
behaviour at all. Bump `metel-interpreter/Cargo.toml`'s `version` to match, in the same
commit as the changelog finalization.

**3. RFC state** —

```bash
python3 docs/internal/rfcs/tools/rfc.py check
```

Reports clean. Any RFC this release implements end-to-end moves to `4-implemented`
(`rfc.py transition <id> --to implemented`) rather than being left at `3-integrated` with
stale "Not yet implemented" spec callouts.

**4. Docs submodule in lockstep** — bump the `docs` pointer to `metel-docs main`'s tip as
part of the release commit. This is the only time `main`'s pointer moves. Since
`metel-docs` is trunk-based, its `main` should already hold everything the changelog and
spec entries were written against; if it doesn't, something bypassed the pair rule.

**5. Spec correctness** — spot-check `docs/public/reference/spec.md` and its linked
sections against the behaviour actually being released, not an aspirational version of it.

**6. Internal docs** — `metel-interpreter/docs/architecture.md`, `typechecker.md`, and
`evaluator.md` reflect the pipeline, inference, construction, runtime, and builtin
behaviour as it now is. Read them against the release's diff, not from memory.

**7. Decision records** — every non-obvious architectural decision, reversal, or
workaround this release introduced has an ADR in `metel-interpreter/docs/decisions/`. A
reversal especially: why a past decision stopped holding is the part that gets lost.

**8. Full suite green on the release commit** — the per-PR gate ran per branch, but not
on this exact tree:

```bash
cd metel-interpreter && cargo test --release
cargo clippy --release --lib -- -W clippy::pedantic
```

Confirm by reading the `test result:` lines and a 0-warning clippy tail.

## Step 3 — Fix findings, then re-run

Every failing item gets fixed on its own issue branch through the normal per-PR flow —
a release does not license committing straight to `develop`. Then re-run the gate.

## Step 4 — Fast-forward and tag

```bash
git checkout main
git merge --ff-only develop
git push origin main
git tag -a $ARGUMENTS -m "$ARGUMENTS: <theme>"
git push origin $ARGUMENTS
```

There is no release merge commit. Because every issue branch fast-forwarded into
`develop`, `main` is by construction a prefix of it, and what a release contained is read
off the tags bracketing the range.

The tag is created on `main` after the fast-forward, never before, and its name must match
the version in the changelog and in `Cargo.toml`.

## Step 5 — Publish

1. Create a Codeberg Release from the tag, with the body sourced from the changelog
   section just finalized — not regenerated separately. The changelog is the single source
   of truth for release notes.
2. If public documentation changed, follow AGENTS.md § Wiki and Public Docs Release
   Workflow (`metel-website` pointer, versioned snapshot) as part of this release, not as
   a later step.
3. Lift the freeze on `develop`.

## Step 6 — Report

```
## $ARGUMENTS — released

**Range:** <prev-tag>..$ARGUMENTS (<N> commits)
**Gate:** all 8 items passed.

### Shipped
- #N: <title>
…

### RFCs reaching implemented
- RFC-NNNN: <title>
…

### Deferred out of this milestone
- #N: <title> → re-milestoned to <version>
…
```

## Notes

- Do not tag before `main` is pushed and verified — check the remote rather than trusting
  the push output, which has reported "Everything up-to-date" against a stale ref before.
- A patch release that turns out to contain a spec change is not a patch release; go back
  to gate item 2.
- If gaps surfaced that need design rather than a fix, prompt the user to run `/new-rfc`.
