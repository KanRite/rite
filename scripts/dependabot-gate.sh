#!/usr/bin/env bash
#
# dependabot-gate.sh MODE [PR] — decide what happens to Dependabot's PRs
# (ticket 247). Run by .github/workflows/dependabot-gate.yml.
#
#   assess  look at the PR (or every open Dependabot PR): label it, raise or
#           keep an issue when it needs a person, turn auto-merge on or off.
#           Never approves. Runs when a PR opens or its CI finishes.
#   apply   the same, and approve a PR that's ready, as the account whose
#           token is in APPROVER_TOKEN (a code owner, so the approval counts).
#           Runs weekly and by hand.
#   closed  a PR was merged or closed: close the issue raised for it.
#
# Looking at every open PR (no PR given), it also raises an issue on an older
# PR that a newer one supersedes. Every issue it raises reaches the KanRite
# board as a TRIAGE ticket, through rite's GitHub issue import.
#
# A PR is ready when every update in it is compatible (a patch, or a minor
# update of a 1.x-or-later version; a major, or a minor of a 0.x version,
# which Cargo counts as breaking, needs a person), no update's new version
# has a security advisory in GitHub's database (RustSec included), and CI's
# rust and shell checks have passed. An advisory holds it: the weekly run
# looks again, and applies it once the advisory is gone. The week's wait
# before a PR exists at all is Dependabot's own cooldown, in dependabot.yml.
#
# DRY_RUN=1 prints what it would do and changes nothing.
set -euo pipefail

REPO="${GITHUB_REPOSITORY:?set GITHUB_REPOSITORY to owner/repo}"
MODE="${1:?usage: dependabot-gate.sh assess|apply|closed [PR]}"
ONLY="${2:-}"

HELD="held: advisory"
PERSON="needs a person"
READY="ready to apply"
ISSUE_LABEL="dependency update"

say() { printf '%s\n' "$*"; }

# Runs a command that changes something, or says what it would run.
act() {
  if [ "${DRY_RUN:-}" = "1" ]; then
    {
      printf 'would run:'
      printf ' %q' "$@"
      printf '\n'
    } >&2
  else
    "$@"
  fi
}

ensure_labels() {
  act gh label create "${HELD}" -R "${REPO}" --color B60205 --force \
    --description "A security advisory names a version this update brings; looked at again weekly" >/dev/null
  act gh label create "${PERSON}" -R "${REPO}" --color FBCA04 --force \
    --description "Not a compatible update, or its checks failed: see the linked issue" >/dev/null
  act gh label create "${READY}" -R "${REPO}" --color 0E8A16 --force \
    --description "Compatible, no advisory, checks passed: applied by the gate" >/dev/null
  act gh label create "${ISSUE_LABEL}" -R "${REPO}" --color 5319E7 --force \
    --description "A Dependabot update that needs a person" >/dev/null
}

# Gives the PR exactly one of the gate's labels.
set_label() {
  local pr="$1" want="$2" label
  for label in "${HELD}" "${PERSON}" "${READY}"; do
    if [ "${label}" = "${want}" ]; then
      act gh pr edit "${pr}" -R "${REPO}" --add-label "${label}" >/dev/null
    elif gh pr view "${pr}" -R "${REPO}" --json labels --jq '.labels[].name' | grep -qxF "${label}"; then
      act gh pr edit "${pr}" -R "${REPO}" --remove-label "${label}" >/dev/null
    fi
  done
}

# The open issue raised for a PR, if there is one.
issue_for() {
  gh issue list -R "${REPO}" --label "${ISSUE_LABEL}" --state open --json number,body \
    --jq ".[] | select(.body | contains(\"<!-- dependabot-pr: $1 -->\")) | .number" | head -1
}

ensure_issue() {
  local pr="$1" title="$2" why="$3"
  if [ -n "$(issue_for "${pr}")" ]; then
    return 0
  fi
  act gh issue create -R "${REPO}" --label "${ISSUE_LABEL}" \
    --title "Dependency update needs a person: ${title}" \
    --body "Dependabot's #${pr} can't be applied automatically:

${why}

Work on it in #${pr}. This issue closes itself when that PR is merged or closed.

<!-- dependabot-pr: ${pr} -->" >/dev/null
  say "#${pr}: raised an issue"
}

# Comments once for each set of advisories that holds a PR.
note_advisories() {
  local pr="$1" text="$2" marker
  marker="<!-- dependabot-gate advisory: $(printf '%s' "${text}" | tr -s ' \n' '  ') -->"
  if gh pr view "${pr}" -R "${REPO}" --json comments --jq '.comments[].body' | grep -qF "${marker}"; then
    return 0
  fi
  act gh pr comment "${pr}" -R "${REPO}" --body "Held: GitHub's advisory database names a version this update brings.

${text}

Nothing is applied while that's so. The gate looks again every week, and applies it once the advisory is gone; a newer, fixed release from Dependabot replaces this PR and is checked the same way.

${marker}" >/dev/null
}

# rust's and shell's state on the PR: success, failed or pending.
ci_state() {
  gh pr view "$1" -R "${REPO}" --json statusCheckRollup --jq '
    [.statusCheckRollup[] | select(.name == "rust" or .name == "shell")] as $c
    | if ($c | length) < 2 then "pending"
      elif any($c[]; (.conclusion // "") as $x
          | $x == "FAILURE" or $x == "CANCELLED" or $x == "TIMED_OUT" or $x == "ACTION_REQUIRED")
        then "failed"
      elif all($c[]; .conclusion == "SUCCESS") then "success"
      else "pending" end'
}

# "name version update-type" for each update in the PR, from the metadata
# Dependabot writes into its commit message.
updates_in() {
  gh api "repos/${REPO}/pulls/$1/commits" --jq '.[0].commit.message' | awk '
    /^updated-dependencies:/ { on = 1; next }
    on && /^\.\.\./ { on = 0 }
    on && /dependency-name:/ { name = $NF }
    on && /dependency-version:/ { ver = $NF; gsub(/"/, "", ver) }
    on && /update-type:/ { print name, ver, $NF }'
}

assess() {
  local pr="$1" branch eco title updates name ver type major ids
  local compatible=1 why="" advisories=""
  branch="$(gh pr view "${pr}" -R "${REPO}" --json headRefName --jq .headRefName)"
  title="$(gh pr view "${pr}" -R "${REPO}" --json title --jq .title)"
  case "${branch}" in
    dependabot/cargo/*) eco=rust ;;
    dependabot/github_actions/*) eco=actions ;;
    *) eco="" ;;
  esac
  updates="$(updates_in "${pr}")"
  if [ -z "${updates}" ]; then
    compatible=0
    why="- Couldn't read which updates it makes."
  fi
  while read -r name ver type; do
    [ -n "${name}" ] || continue
    major="${ver%%.*}"
    case "${type}" in
      version-update:semver-patch) ;;
      version-update:semver-minor)
        if [ "${major}" = "0" ]; then
          compatible=0
          why="${why}- ${name} ${ver}: a minor update of a 0.x version, which Cargo counts as breaking."$'\n'
        fi
        ;;
      *)
        compatible=0
        why="${why}- ${name} ${ver}: a major update."$'\n'
        ;;
    esac
    if [ -n "${eco}" ]; then
      ids="$(gh api -X GET advisories -f ecosystem="${eco}" -f affects="${name}@${ver}" \
        --jq '[.[] | select(.withdrawn_at == null) | .ghsa_id] | join(", ")')"
      if [ -n "${ids}" ]; then
        advisories="${advisories}- ${name} ${ver}: ${ids}"$'\n'
      fi
    fi
  done <<< "${updates}"

  local ci
  ci="$(ci_state "${pr}")"
  if [ "${compatible}" = "0" ]; then
    say "#${pr}: needs a person (not compatible)"
    set_label "${pr}" "${PERSON}"
    act gh pr merge "${pr}" -R "${REPO}" --disable-auto >/dev/null 2>&1 || true
    ensure_issue "${pr}" "${title}" "${why}"
  elif [ -n "${advisories}" ]; then
    say "#${pr}: held by an advisory"
    set_label "${pr}" "${HELD}"
    act gh pr merge "${pr}" -R "${REPO}" --disable-auto >/dev/null 2>&1 || true
    note_advisories "${pr}" "${advisories}"
  elif [ "${ci}" = "failed" ]; then
    say "#${pr}: needs a person (checks failed)"
    set_label "${pr}" "${PERSON}"
    ensure_issue "${pr}" "${title}" "- Compatible, but its checks (rust, shell) failed."
  elif [ "${ci}" = "pending" ]; then
    say "#${pr}: compatible, no advisory; waiting for its checks"
  else
    say "#${pr}: ready"
    set_label "${pr}" "${READY}"
    act gh pr merge "${pr}" -R "${REPO}" --auto --squash >/dev/null 2>&1 || true
    if [ "${MODE}" = "apply" ]; then
      approve "${pr}"
    fi
  fi
}

# Approves the PR's current head as the token's owner, once.
approve() {
  local pr="$1" head me
  if [ -z "${APPROVER_TOKEN:-}" ]; then
    say "#${pr}: no APPROVER_TOKEN, so it waits for a code owner's approval"
    return 0
  fi
  head="$(gh pr view "${pr}" -R "${REPO}" --json headRefOid --jq .headRefOid)"
  me="$(GH_TOKEN="${APPROVER_TOKEN}" gh api user --jq .login)"
  if gh api "repos/${REPO}/pulls/${pr}/reviews" \
    --jq "[.[] | select(.user.login == \"${me}\" and .state == \"APPROVED\" and .commit_id == \"${head}\")] | length" \
    | grep -qx '0'; then
    GH_TOKEN="${APPROVER_TOKEN}" act gh pr review "${pr}" -R "${REPO}" --approve \
      --body "Compatible, a week old, no security advisory, and its checks passed: applied by the Dependabot gate." >/dev/null
    say "#${pr}: approved as ${me}"
  fi
}

# Two open PRs updating the same dependency: Dependabot normally closes the
# older one itself, but not one someone else has pushed to. Raise an issue on
# the older one (which reaches the board as a TRIAGE ticket) to close it.
check_superseded() {
  local seen="" pr name newer
  for pr in $(printf '%s\n' "$@" | sort -n); do
    while read -r name _; do
      [ -n "${name}" ] || continue
      seen="${seen}${name} ${pr}"$'\n'
    done <<< "$(updates_in "${pr}")"
  done
  for pr in "$@"; do
    while read -r name _; do
      [ -n "${name}" ] || continue
      newer="$(printf '%s' "${seen}" | awk -v n="${name}" -v p="${pr}" '$1 == n && $2 > p { print $2 }' | sort -n | tail -1)"
      if [ -n "${newer}" ]; then
        say "#${pr}: superseded by #${newer} (${name})"
        ensure_issue "${pr}" "$(gh pr view "${pr}" -R "${REPO}" --json title --jq .title)" \
          "- Superseded: #${newer} updates ${name} too, to a newer version. Close this one; Dependabot won't, because it has been pushed to by someone else."
        break
      fi
    done <<< "$(updates_in "${pr}")"
  done
}

closed() {
  local pr="$1" issue
  issue="$(issue_for "${pr}")"
  if [ -n "${issue}" ]; then
    act gh issue close "${issue}" -R "${REPO}" --comment "#${pr} was merged or closed." >/dev/null
    say "closed #${issue} for #${pr}"
  fi
}

case "${MODE}" in
  closed)
    closed "${ONLY:?closed needs the PR number}"
    ;;
  assess|apply)
    ensure_labels
    if [ -n "${ONLY}" ]; then
      prs="${ONLY}"
    else
      prs="$(gh pr list -R "${REPO}" --author app/dependabot --state open --json number --jq '.[].number')"
    fi
    for pr in ${prs}; do
      assess "${pr}"
    done
    if [ -z "${ONLY}" ]; then
      # shellcheck disable=SC2086 # the numbers are words on purpose
      check_superseded ${prs}
    fi
    ;;
  *)
    echo "usage: dependabot-gate.sh assess|apply|closed [PR]" >&2
    exit 2
    ;;
esac
