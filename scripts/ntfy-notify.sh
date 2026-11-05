#!/usr/bin/env bash
#
# ntfy-notify.sh TITLE MESSAGE TAG LINK — sends a push notification through
# ntfy (ticket 321). Run by scripts/workflow-report.sh, so a workflow failing,
# or passing again, reaches a phone as well as the board.
#
# TAG is one of ntfy's emoji names (x, white_check_mark). LINK is what opening
# the notification goes to.
#
# From the environment:
#   NTFY_TOPIC   the topic. On ntfy.sh anyone who knows it can read and post,
#                so it is long, random and a secret, never in a URL or a log.
#                ntfy refuses one over 64 characters
#   NTFY_SERVER  defaults to https://ntfy.sh
#   NTFY_STRICT  1 fails when nothing could be sent (the self-test); otherwise
#                that is a warning, because the issue the report raises
#                matters more than its echo on a phone
set -euo pipefail

TITLE="${1:?usage: ntfy-notify.sh TITLE MESSAGE TAG LINK}"
MESSAGE="${2:?usage: ntfy-notify.sh TITLE MESSAGE TAG LINK}"
TAG="${3:?usage: ntfy-notify.sh TITLE MESSAGE TAG LINK}"
LINK="${4:?usage: ntfy-notify.sh TITLE MESSAGE TAG LINK}"

# Says why nothing was sent: an error under NTFY_STRICT, else a warning.
fail() {
  if [ "${NTFY_STRICT:-}" = "1" ]; then
    echo "::error::$1"
    exit 1
  fi
  echo "::warning::$1"
  exit 0
}

if [ -z "${NTFY_TOPIC:-}" ]; then
  fail "NTFY_TOPIC isn't set, so no notification was sent"
fi

if [ "${#NTFY_TOPIC}" -gt 64 ]; then
  fail "NTFY_TOPIC is longer than ntfy's limit of 64 characters"
fi

# Published as JSON to the server's root, on standard input, so the topic
# appears in neither the URL nor the command line.
if ! jq -n --arg topic "${NTFY_TOPIC}" --arg title "${TITLE}" \
  --arg message "${MESSAGE}" --arg tag "${TAG}" --arg click "${LINK}" \
  '{topic: $topic, title: $title, message: $message, tags: [$tag], click: $click}' \
  | curl --silent --show-error --fail --max-time 20 --output /dev/null \
    --data-binary @- "${NTFY_SERVER:-https://ntfy.sh}"; then
  fail "couldn't send the notification to ntfy"
fi
echo "sent to ntfy"
