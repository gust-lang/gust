# Releasing

Cutting a `vX.Y.Z` tag here triggers the entire public-facing release chain, end to
end, across three repositories. This document describes that chain; the workflows
themselves are `.github/workflows/release.yml` here and
[`metel-website`'s `deploy.yml`](https://github.com/metel-lang/metel-website/blob/main/.github/workflows/deploy.yml).

## The chain

```
metel-core tag vX.Y.Z pushed
        │
        ▼
release.yml (here, Stage A)
  1. checks out this repo's docs submodule (metel-docs-internal, private,
     read-only DOCS_REPO_TOKEN — the same credential #619's CI check uses)
  2. mirrors that submodule's public/ tree into metel-docs (public), commits
     only if content actually changed, pushes to main
  3. in a fresh metel-website checkout: bumps its docs submodule pointer to
     that commit, runs `docusaurus docs:version X.Y.Z`, commits the generated
     versioned_docs/ + versioned_sidebars/ + versions.json (skipped if nothing
     changed), pushes to main
  4. tags that commit vX.Y.Z in metel-website — skipped if the tag already
     exists there (safe to re-run this whole workflow after fixing a failure)
        │
        ▼  (a real tag push — GitHub's own trigger, not a synthetic dispatch)
deploy.yml (metel-website, Stage B)
  1. builds the site once
  2. deploys that build to Vercel staging, posts the URL to the run summary
  3. pauses on metel-website's "production" GitHub Environment for a
     required-reviewer approval
  4. on approval: promotes that exact staged build (no rebuild) to production
  5. verifies both the staging URL and https://metel-lang.org are reachable
```

## Why it's split this way

Each step writes to exactly one repository, using a credential scoped to that repo
alone:

| Secret | Lives in | Scope |
|---|---|---|
| `DOCS_REPO_TOKEN` | metel-core | Read-only, `metel-docs-internal` only (already exists, from #619) |
| `DOCS_PUBLIC_TOKEN` | metel-core | Write, `metel-docs` only |
| `WEBSITE_TOKEN` | metel-core | Write, `metel-website` only |
| `VERCEL_TOKEN` / `VERCEL_ORG_ID` / `VERCEL_PROJECT_ID` | metel-website | Vercel deploy access for this project |

No single credential spans all three repositories. `release.yml` triggers
`deploy.yml` by pushing a real tag rather than a synthetic `repository_dispatch`
event, so `metel-website`'s pipeline stays independently triggerable — pushing a
`vX.Y.Z` tag directly to `metel-website` runs the exact same build-and-deploy
pipeline on its own, useful for testing that side without cutting a `metel-core`
release at all.

Production promotion is gated on a human approval (`metel-website`'s `production`
Environment) rather than fully unattended, specifically to preserve the review
checkpoint the old manual process had implicitly (someone ran `docs:version`, looked
at it, then deployed) — full automation shouldn't mean zero review before something
goes live on `metel-lang.org`.

## Failure handling

There is no cross-repo rollback. If the chain fails partway — e.g. the `metel-docs`
sync succeeds but the `metel-website` step errors — the run fails loudly (visible via
`gh run list --repo metel-lang/metel-core`) and stops there, leaving a partial but
inspectable state rather than a silent inconsistency. Every write step diffs before
committing, and the final tag step checks for the tag's existence first, so re-running
`release.yml` on the same tag after fixing whatever broke is safe: steps that already
completed become no-ops, and only the step that failed (and anything after it) does
real work the second time.

## Provisioning the required secrets

None of these can be created by an assistant holding only a personal access token
scoped to this repo — each needs to be set up by hand, once:

- **`DOCS_PUBLIC_TOKEN`**: a GitHub fine-grained PAT, "Only select repositories" →
  `metel-docs`, `Contents: Read and write`. Store as a `metel-core` repo secret.
- **`WEBSITE_TOKEN`**: a GitHub fine-grained PAT, "Only select repositories" →
  `metel-website`, `Contents: Read and write`. Store as a `metel-core` repo secret.
- **`VERCEL_TOKEN`**: from the Vercel dashboard (Account Settings → Tokens), scoped to
  the project backing `metel-website` if Vercel's token scoping allows it. Store as a
  `metel-website` repo secret.
- **`VERCEL_ORG_ID`** / **`VERCEL_PROJECT_ID`**: from that same project's Vercel
  dashboard settings, or by running `vercel link` locally against it once and reading
  `.vercel/project.json`. Store as `metel-website` repo secrets.
- **`metel-website`'s `production` Environment**: Settings → Environments → New
  environment named `production`, with at least one required reviewer added under
  "Deployment protection rules".
