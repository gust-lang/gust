---
name: strategy-cycle
description: Run a strategic-overview cycle for this project — check triggers against reality, re-verify priorities, argue the open questions, and record the result. Use when the user asks for a strategic overview, a strategy cycle, a strategic review, or to check the watch list / triggers / objectives.
user-invocable: true
allowed-tools:
  - Read
  - Write
  - Edit
  - Bash
  - Grep
  - Glob
  - AskUserQuestion
---

# Strategic overview cycle

The strategy corpus lives in the `docs/` submodule at `docs/reports/strategy/`, as typed
entity files (one per goal, priority, recommendation, trigger, heuristic) plus one
directory per cycle. `OBJECTIVES.md` and `HEURISTICS.md` are **generated views** — never
edit them; edit the entity files and run `render`.

All mechanical work is `docs/reports/strategy/tools/strategy.py`. Run it from the
submodule root (`cd docs`). **Everything that can be checked is checked by the script;
your job is the part that cannot be — the argument.**

```bash
cd docs
python3 reports/strategy/tools/strategy.py check      # validate; non-zero on any problem
python3 reports/strategy/tools/strategy.py cycle prep # step 0's checkpoint
python3 reports/strategy/tools/strategy.py stats      # size/noise metrics
```

`reports/strategy/PROCESS.md` remains the authority on *why* each rule exists. This skill
is the procedure; where they disagree, PROCESS.md wins and should be corrected.

---

## Step 0 — Steering checkpoint. Always runs, even on a quiet cycle.

```bash
cd docs && python3 reports/strategy/tools/strategy.py cycle prep
python3 public/rfcs/tools/rfc.py cycle-prep --diff
```

Read both. Then state to the operator, in a few sentences:

- what changed since the last cycle that likely matters;
- **which ranked priority has gone longest with no engineering movement, and since when**
  (`cycle prep` prints `last_moved` per priority — use it, don't reconstruct it);
- **which `proposed` goal has waited longest on its own recommendation**;
- any recommendation open 2+ cycles (`cycle prep` flags these).

**Then ask whether the operator wants to redirect before you proceed.** Do not run the
cycle and let them correct the finished artifact — that is strictly more expensive. If
they redirect, record it as an operator directive (`parts/directives.md`) before
continuing.

## Step 1 — Verify, against primary sources only

For every open trigger, check its falsifier against **the actual artifact**, never against
a prior cycle's narrative or an entity's own frontmatter:

| Claim about | Check with |
|---|---|
| grammar/code behaviour | read the `.pest`/`.rs`, or run the built binary on a constructed `.mtl` |
| an RFC's stage | its file's own directory + frontmatter; `rfc.py index --rebuild-registry` before quoting tallies |
| the tracker | `gh issue list --milestone …` directly |
| "merged"/"shipped" | `git log`, `git merge-base --is-ancestor`, `git tag` |
| "unchanged since X" | `git log -- <path>` |
| a date about the project | `git log --reverse` — **not** a date in a process document (H07) |

Record each result as a Finding in `cycles/<date>/records.md` with `verified:`
`checked` | `reasoned` | `unverifiable-in-time`, and the exact `via:` source. **If you
could not settle something, say so with `unverifiable-in-time`** rather than presenting it
at the same confidence as a checked claim.

## Step 2 — Scaffold and write

```bash
cd docs && python3 reports/strategy/tools/strategy.py cycle new
```

This creates `cycles/<today>/` with `cycle.md`, `records.md`, `at-a-glance.md`.

**Fill `records.md` first, then write the prose from it.** Records are the substrate; the
report is a view. Every substantive assertion in the prose cites an `E-`/`F-` id, and
`check` fails on a citation that resolves to nothing.

The report is a **strategic meeting, not a status report** — organized by *topic*, not by
epistemic status. An agenda of 2–4 genuinely open questions, then one discussion
subsection each, and every subsection carries all four of:

1. **the strongest case each way** — the opposing case at its best, not a strawman;
2. **what the evidence supports** — citing record ids, separating checked from reasoned;
3. **where this lands** — a position, or an honest "parked, and here's the blocker";
4. **what would change this** — the observation that would move the conclusion.

A subsection missing any of the four is not finished. Do not pad to four agenda items; a
quiet cycle has one.

`cycle.md`'s `summary:` frontmatter becomes the review-log row. **One cycle produces one
row** — that is enforced by the layout, not by discipline. Keep it to a sentence or two.

## Step 3 — Update state

- **New trigger:** `strategy.py new trigger --goal <slug> --falsifier "<what would settle it>"`.
  `check` rejects an open trigger with no falsifier.
- **Close one:** `strategy.py close T0NN "<resolution>"` (add `--partial` for a mixed
  result). Closure requires the **strongest available** resolution, not the minimum that
  satisfies the falsifier's letter.
- **New recommendation:** `strategy.py new recommendation`. A recommendation proposes an
  action *now*; a trigger watches for a future condition. Don't conflate them.
- **Decide one:** `strategy.py decide R0NN approved|deferred|rejected "<reason>"`. Only
  the operator decides; you never resolve one silently.
- **Heuristic:** `strategy.py new heuristic --title "<the rule>"` — and only if the
  principle is *genuinely new and general*. Two in one day means the bar is too low.
- **Priorities:** edit `priorities/P0N.md` frontmatter, including `last_moved`.

Then check each substantive new fact against **every** `HEURISTICS.md` entry as an
explicit pass — state which applied, which didn't, and why. This is a checklist, not
free recall; it exists because relying on recall is where consistency broke down.

## Step 4 — Render, check, commit

```bash
cd docs
python3 reports/strategy/tools/strategy.py archive --apply   # if check asks for it
python3 reports/strategy/tools/strategy.py render
python3 reports/strategy/tools/strategy.py check             # must be clean
python3 reports/strategy/tools/strategy.py stats             # sanity-check the size
```

Commit the entity files and the rendered views together in the submodule, then bump
metel-core's `docs` pointer on an issue branch — never directly on `develop`/`main`.

## Keep it small

This process was judged "too noisy" on 2026-08-10, after one day added 523 lines to
`OBJECTIVES.md`. The causes, so they don't recur:

- **Do not append a "durable lesson" paragraph to every correction.** State what was
  wrong and the fix. A lesson becomes a heuristic only if it generalizes.
- **Do not restate an entity's original text when closing it.** The file has history.
- **Run `archive --apply` when `check` asks.** The archiving rule existed unrun for its
  whole life before being automated.
- **`stats` before committing.** If an entity has outgrown the mean by several multiples,
  cut it rather than adding to it.

## Boundaries

- **This is not a venue for design work.** A cycle *evaluates*; when research surfaces a
  real design finding, it gets fixed in the RFC it belongs to and the cycle reports that
  it did. A discussion item resolving into "and here is the design" has overrun.
- **A dated cycle is event-based, not calendar-based, and stays human-triggered.** If a
  natural trigger fires while you are doing something else, name it and let the operator
  decide — don't run a cycle unasked, and don't stay silent about noticing.
- **Promoting a goal `proposed` → `active` is only ever an operator decision.**
- **`mayak` (Goal 5) uses this process as its validation instrument** (Trigger 36). Every
  process change states which project's need it serves; the test is *"would this change
  have been made if `mayak` did not exist?"*
