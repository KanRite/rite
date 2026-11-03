#!/usr/bin/env bash
#
# check-failure-reporter.sh — every workflow is one workflow-failures.yml
# reports on (ticket 348). `workflow_run` has to name the workflows it watches,
# so a new workflow would otherwise be left out without anyone noticing, and a
# red one would never reach the board. Run in CI's shell job.
#
# Compares each `name:` in .github/workflows/*.yml (except the reporter's own,
# and a workflow that only runs inside others, `on: workflow_call` alone, such
# as board-page.yml: it never finishes a run of its own for `workflow_run` to
# see, and its failure fails the workflow that called it) with the list under
# `workflows:` in workflow-failures.yml.
set -euo pipefail

dir="${1:-.github/workflows}"
reporter="${dir}/workflow-failures.yml"

# A YAML scalar without its quotes and trailing space.
unquote() { sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/^"\(.*\)"$/\1/'; }

# Whether a workflow's only trigger is workflow_call. Its triggers are what
# follows `on:` on the same line (`on: push`), or the keys indented under it.
called_only() {
  local triggers
  triggers="$(awk '
    /^on:/ { on = 1; sub(/^on:[[:space:]]*/, ""); if ($0 != "") print; next }
    on && /^[^[:space:]#]/ { exit }
    on && /^  [a-z_]+:/ { sub(/^  /, ""); sub(/:.*/, ""); print }
  ' "$1")"
  [ "${triggers}" = "workflow_call" ]
}
declared="$(for file in "${dir}"/*.yml; do
  called_only "${file}" && continue
  grep -h '^name:' "${file}" | sed 's/^name:[[:space:]]*//'
done | unquote | sort -u)"
declared="$(printf '%s\n' "${declared}" | grep -vx 'Workflow failures' || true)"

# The list items between `workflows:` and `types:`.
listed="$(awk '/^    workflows:/{on=1; next} /^    types:/{on=0} on && /^      - /{sub(/^      - /, ""); print}' "${reporter}" | unquote | sort -u)"

status=0
missing="$(comm -23 <(printf '%s\n' "${declared}") <(printf '%s\n' "${listed}"))"
extra="$(comm -13 <(printf '%s\n' "${declared}") <(printf '%s\n' "${listed}"))"
if [ -n "${missing}" ]; then
  echo "workflow-failures.yml doesn't report on:" >&2
  printf '%s\n' "${missing}" | sed 's/^/  /' >&2
  echo "add each to its 'workflows:' list, so a failure reaches the board" >&2
  status=1
fi
if [ -n "${extra}" ]; then
  echo "workflow-failures.yml lists workflows that don't exist:" >&2
  printf '%s\n' "${extra}" | sed 's/^/  /' >&2
  status=1
fi
[ "${status}" -eq 0 ] && echo "every workflow is reported on: $(printf '%s' "${declared}" | tr '\n' ',' | sed 's/,$//;s/,/, /g')"
exit "${status}"
