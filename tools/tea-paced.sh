#!/usr/bin/env bash
# Generic retry/backoff wrapper for any `tea` command, to survive Codeberg's
# anti-spam rate limits on issue/comment creation (observed: ~5 issue creates
# or ~15-16 comments per 5-minute window per account — an abuse guard, not
# the general API rate limit, and not something a paid/membership tier lifts
# (Codeberg is a nonprofit, donation/membership-funded, no premium tiers).
#
# Usage: tea-paced.sh <any tea subcommand and args>
#   tools/tea-paced.sh issues create --title "..." --login X --repo Y
#   tools/tea-paced.sh comments add 42 "..." --login X --repo Y
#
# Env vars (all optional):
#   TEA_PACED_MAX_RETRIES=6    retries before giving up (default 6)
#   TEA_PACED_BACKOFF=90       seconds to wait after a rate-limit hit (default 90)
#
# On success, prints tea's own output and exits 0. On repeated rate-limiting
# past TEA_PACED_MAX_RETRIES, exits 1 with the last error. Any non-rate-limit
# error from tea is not retried — it's printed and the wrapper exits 1
# immediately, since retrying a real error (bad title, wrong repo, etc.)
# just wastes the retry budget on something that will never succeed.
#
# For bulk loops (creating many issues/comments back to back), still add a
# pause between individual calls in the calling loop — this wrapper only
# handles recovering from a rate-limit hit, it doesn't pre-emptively pace
# calls that haven't happened yet.

set -uo pipefail

MAX_RETRIES="${TEA_PACED_MAX_RETRIES:-6}"
BACKOFF="${TEA_PACED_BACKOFF:-90}"

if [[ $# -eq 0 ]]; then
  echo "usage: tea-paced.sh <tea subcommand and args...>" >&2
  exit 2
fi

attempt=0
while :; do
  attempt=$((attempt + 1))
  out=$(tea "$@" 2>&1)
  status=$?

  if [[ $status -eq 0 ]]; then
    echo "$out"
    exit 0
  fi

  if echo "$out" | grep -qi "rate limited"; then
    if (( attempt >= MAX_RETRIES )); then
      echo "tea-paced: giving up after $attempt attempts (still rate limited)" >&2
      echo "$out" >&2
      exit 1
    fi
    echo "tea-paced: rate limited, backing off ${BACKOFF}s (attempt $attempt/$MAX_RETRIES)" >&2
    sleep "$BACKOFF"
    continue
  fi

  # Not a rate-limit error — don't retry, surface it immediately.
  echo "$out" >&2
  exit "$status"
done
