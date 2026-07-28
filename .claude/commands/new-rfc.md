# /new-rfc

Create a new RFC. The markdown file in `metel-docs` is the source of truth for both the
RFC's content **and** its lifecycle state — there is no tracker mirroring it.

**Arguments:** `$ARGUMENTS` — the RFC title, e.g. `Array literal syntax`

`docs/internal/rfcs/PROCESS.md` is the sole authority on the RFC lifecycle. This command
mechanises opening one; read PROCESS.md for anything beyond that.

## Steps

1. **Check it doesn't already exist.** Read `docs/internal/rfcs/INDEX.md` — the thematic
   snapshot of every RFC by cluster and status. An RFC covering this ground, or a settled
   decision bundled inside a broader RFC, is common enough that this step earns its place.

   If an existing draft bundles a *settled* decision with genuinely open ones, splitting
   the settled part into its own RFC is usually better than adding a new one alongside —
   that is what unblocked RFC-0126 out of RFC-0124.

2. **Create the file** with the tool, which assigns the number, derives the slug, writes
   the frontmatter, and runs its own overlap check against existing RFCs:

```bash
python3 docs/internal/rfcs/tools/rfc.py new "$ARGUMENTS" \
  --description "<one-line summary for the overlap check>"
```

   It lands at `docs/internal/rfcs/0-draft/rfc-NNNN-<slug>.md` with `status: draft`.
   Do not hand-number or hand-place an RFC file — the directory and the frontmatter must
   agree, and `rfc.py check` enforces that.

3. **Write the content**, filling sections from conversation context. Leave a section
   blank only when there is genuinely insufficient information — an empty Alternatives
   Considered usually means the design wasn't pressured, not that there were none.

   Include, at minimum: what problem this solves, the proposal itself, alternatives with
   why they lose, and the open questions honestly enumerated. Prior art from comparable
   languages (Rust, Zig, C++, Go, Swift) is worth a table when the decision has one.

4. **Validate.**

```bash
python3 docs/internal/rfcs/tools/rfc.py check
```

   Clean means frontmatter matches directory, no dangling references, no duplicate ids.

5. **Commit in the submodule, then bump the pointer.** `metel-docs` is trunk-based —
   commit straight to its `main`:

```bash
git -C docs status -sb          # confirm on main, not detached
git -C docs commit -m "docs(RFC-NNNN): add draft RFC — <title>"
git -C docs push origin main
```

   Then commit the submodule pointer bump in this repo, on an issue branch — never
   directly on `develop` or `main`.

## Lifecycle

Seven stages, each one a directory. The **directory is the state**; frontmatter `status`
must match it. Transition with the tool, never by hand:

```bash
python3 docs/internal/rfcs/tools/rfc.py transition <id> --to <stage>
```

| Directory | `status` | Meaning |
|---|---|---|
| `0-draft/` | `draft` | Being written |
| `1-under-review/` | `under-review` | Ready for evaluation |
| `2-accepted/` | `accepted` | Design settled; not yet in the spec |
| `3-integrated/` | `integrated` | Merged into `docs/public/reference/spec/`, worked examples checked against everything already integrated |
| `4-implemented/` | `implemented` | Implemented and shipped |
| `5-superseded/` | `superseded` | Replaced by a later RFC |
| `6-refused/` | `refused` | Refused, with the decision recorded |

- **Implementation does not start below `3-integrated`.** That stage is the spec being
  updated and the examples being checked — not a formality.
- `transition --to integrated` **refuses to run without `--tracking <issue-url>`**. From
  `3-integrated` onward the frontmatter carries `impl_status`
  (`not-started`/`in-progress`/`implemented`) and `impl_tracking` (the Codeberg issue).
  An RFC reaches integrated only with a real implementation issue behind it.
- When implementation lands, `transition <id> --to implemented` also sets
  `impl_status: implemented`.

## Notes

- There is **no** mirrored RFC status on an issue, no custom property, and nothing to keep
  in sync in the other direction. The file is the record — this is a deliberate
  simplification versus how Plane was used, not an oversight.
- An RFC gets a Codeberg issue only when it reaches `3-integrated` and needs real
  implementation tracked: one issue per RFC, or per tightly-coupled cluster.
- A `**Target:** vX.Y.0` in the file names the intended release; the issue's milestone is
  what the release gate actually reads.
- After a batch of RFC edits, `rfc.py index --rebuild-registry` refreshes `INDEX.md`.
