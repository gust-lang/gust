#!/usr/bin/env bash
# Fail if a workflow, tools/ script, or slash command in this repo isn't named in
# PROCESSES.md -- the inventory metel-core#687 exists to keep from silently going
# stale again. Checks by filename only: it doesn't verify the description is
# accurate, just that nothing added here is invisible to the one document meant to
# list it all.
#
# This can only see metel-core's own files. metel-docs-internal's and
# metel-website's workflows aren't reachable from this repo's CI on every push --
# PROCESSES.md's "Keeping this current" section covers those two by review instead.
#
# Usage: tools/check_inventory.sh

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

doc="PROCESSES.md"
if [[ ! -f "$doc" ]]; then
    echo "error: $doc not found" >&2
    exit 1
fi

missing=0
check() {
    local path="$1"
    local name
    name="$(basename "$path")"
    if ! grep -qF "$name" "$doc"; then
        echo "MISSING  $path is not mentioned in $doc"
        missing=$((missing + 1))
    fi
}

for f in .github/workflows/*.yml; do
    [[ -e "$f" ]] || continue
    check "$f"
done
for f in tools/*.py tools/*.sh; do
    [[ -e "$f" ]] || continue
    check "$f"
done
for f in .claude/commands/*.md; do
    [[ -e "$f" ]] || continue
    check "$f"
done
# Skills were a blind spot until 2026-08-11: the strategy-cycle skill was added and this
# check couldn't see it. A skill encodes a whole procedure, so it is exactly the kind of
# thing "what would break if I changed X" needs to find. Checked by directory name, since
# every skill's file is SKILL.md and basenames would all collide.
for d in .claude/skills/*/; do
    [[ -e "$d/SKILL.md" ]] || continue
    name="$(basename "$d")"
    if ! grep -qF "$name" "$doc"; then
        echo "MISSING  ${d}SKILL.md (skill '$name') is not mentioned in $doc"
        missing=$((missing + 1))
    fi
done

if (( missing > 0 )); then
    echo
    echo "$missing file(s) exist in this repo but aren't listed in $doc."
    echo "Add them (what it does, what triggers it, what it reads/writes, which secret) before merging."
    exit 1
fi
echo "All workflows, tools/ scripts, and slash commands are listed in $doc."
