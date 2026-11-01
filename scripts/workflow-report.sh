#!/usr/bin/env bash
#
# workflow-report.sh failed|passed — what a finished workflow leaves on GitHub
# (ticket 348). Run by .github/workflows/workflow-failures.yml.
#
#   failed  open an issue saying the workflow failed on that branch or tag, or
#           comment on the one already open, so a workflow that stays red is
#           one issue, not one a run.
#   passed  close that issue, saying a later run passed.
#
# The issue reaches the board as a TRIAGE ticket through rite's GitHub issue
# import, which is how a red workflow gets noticed. It says nothing about a
# workflow that never runs: nothing runs then, so nothing can report it.
#
# What it is told comes in the environment, never on a command line it builds:
# a branch name is data.
#   GITHUB_REPOSITORY  owner/repo
#   WORKFLOW           the workflow's name
#   BRANCH             the branch or tag it ran on
#   EVENT              what started it (push, schedule, workflow_dispatch)
#   RUN_URL            the run's address
#
# DRY_RUN=1 prints what it would do and changes nothing. GH_TOKEN must be able
# to write issues (in CI, the repository secret ISSUE_GENERATOR_TOKEN).
set -euo pipefail

MODE="${1:?usage: workflow-report.sh failed|passed}"
REPO="${GITHUB_REPOSITORY:?set GITHUB_REPOSITORY to owner/repo}"
WORKFLOW="${WORKFLOW:?set WORKFLOW to the workflow name}"
BRANCH="${BRANCH:?set BRANCH to the branch or tag}"
EVENT="${EVENT:-unknown}"
RUN_URL="${RUN_URL:?set RUN_URL to the address of the run}"
TITLE="Workflow failed: ${WORKFLOW} (${BRANCH})"
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
      body="The ${WORKFLOW} workflow failed on ${BRANCH} (started by ${EVENT}): ${RUN_URL}

Read the log with \`gh run view --log-failed\` on that run.

This issue closes itself when a later run of ${WORKFLOW} on ${BRANCH} passes. It was raised by workflow-failures.yml, which does this for every workflow that runs on main, on a schedule or for a release."
      act gh issue create --repo "${REPO}" --title "${TITLE}" --body "${body}"
    fi
    ;;
  passed)
    if [ -n "${number}" ]; then
      act gh issue close "${number}" --repo "${REPO}" \
        --comment "A later run passed: ${RUN_URL}"
    else
      printf 'no open issue to close\n'
    fi
    ;;
  *)
    echo "usage: workflow-report.sh failed|passed" >&2
    exit 2
    ;;
esac
