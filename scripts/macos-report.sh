#!/usr/bin/env bash
#
# macos-report.sh failed|passed — what a scheduled macOS run leaves on GitHub
# (ticket 345). Run by .github/workflows/macos.yml after its tests.
#
#   failed  open an issue saying the run failed, or comment on the one already
#           open, so a run that stays red every Monday is one issue, not many.
#   passed  close that issue, saying the run passed.
#
# The issue reaches the board as a TRIAGE ticket through rite's GitHub issue
# import, which is how a red Monday run gets noticed. It says nothing about a
# run that never starts: nothing runs then, so nothing can report it.
#
# DRY_RUN=1 prints what it would do and changes nothing. GH_TOKEN must be able
# to write issues; GITHUB_REPOSITORY and RUN_URL say which repository and run.
set -euo pipefail

MODE="${1:?usage: macos-report.sh failed|passed}"
REPO="${GITHUB_REPOSITORY:?set GITHUB_REPOSITORY to owner/repo}"
RUN_URL="${RUN_URL:?set RUN_URL to the address of the run}"
TITLE="macOS: the scheduled run failed"
export TITLE

# Runs a command that changes something, or says what it would run.
act() {
  if [ "${DRY_RUN:-}" = "1" ]; then
    printf 'would run: %s\n' "$*"
  else
    "$@"
  fi
}

# The number of the open issue with this title, if there is one.
open_issue() {
  gh issue list --repo "${REPO}" --state open --limit 100 --json number,title \
    --jq '.[] | select(.title == $ENV.TITLE) | .number' | head -n 1
}

number="$(open_issue)"

case "${MODE}" in
  failed)
    if [ -n "${number}" ]; then
      act gh issue comment "${number}" --repo "${REPO}" \
        --body "Failed again: ${RUN_URL}"
    else
      body="The scheduled macOS run (macos.yml, Mondays) failed: ${RUN_URL}

Read the log with \`gh run view --log-failed\` on that run. A failure on a Monday with nothing changed on main is usually a macOS image or tool change: a new \`macos-latest\`, Homebrew's gnu-sed or coreutils, or a Rust or Trunk release.

This issue closes itself when a scheduled run passes."
      act gh issue create --repo "${REPO}" --title "${TITLE}" --body "${body}"
    fi
    ;;
  passed)
    if [ -n "${number}" ]; then
      act gh issue close "${number}" --repo "${REPO}" \
        --comment "A scheduled run passed: ${RUN_URL}"
    else
      printf 'no open issue to close\n'
    fi
    ;;
  *)
    echo "usage: macos-report.sh failed|passed" >&2
    exit 2
    ;;
esac
