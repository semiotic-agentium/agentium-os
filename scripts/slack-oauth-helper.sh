#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

usage() {
  cat <<'USAGE'
Slack OAuth helper (read-only app install flow)

Usage:
  scripts/slack-oauth-helper.sh install-url [--state <value>] [--scopes <csv>] [--user-scopes <csv>]
  scripts/slack-oauth-helper.sh exchange-code --code <oauth_code>

Environment:
  SLACK_APP_CLIENT_ID        Required for both commands
  SLACK_APP_CLIENT_SECRET    Required for exchange-code
  SLACK_REDIRECT_URI         Required for both commands
  SLACK_OAUTH_BASE_URL       Optional (default: https://slack.com)

Optional scope defaults:
  scopes      (bot scopes):   channels:read,groups:read,im:read,mpim:read,channels:history,groups:history,im:history,mpim:history,users:read
  user_scopes (user scopes):  channels:read,groups:read,im:read,mpim:read,channels:history,groups:history,im:history,mpim:history,users:read,search:read
USAGE
}

require_cmd() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "Required command not found in PATH: $cmd" >&2
    exit 1
  fi
}

urlencode() {
  jq -rn --arg v "$1" '$v|@uri'
}

command="${1:-}"
if [ -z "$command" ]; then
  usage
  exit 1
fi
shift || true

require_cmd jq
require_cmd curl

SLACK_OAUTH_BASE_URL="${SLACK_OAUTH_BASE_URL:-https://slack.com}"
SLACK_APP_CLIENT_ID="${SLACK_APP_CLIENT_ID:-}"
SLACK_APP_CLIENT_SECRET="${SLACK_APP_CLIENT_SECRET:-}"
SLACK_REDIRECT_URI="${SLACK_REDIRECT_URI:-}"

default_scopes="channels:read,groups:read,im:read,mpim:read,channels:history,groups:history,im:history,mpim:history,users:read"
default_user_scopes="channels:read,groups:read,im:read,mpim:read,channels:history,groups:history,im:history,mpim:history,users:read,search:read"

case "$command" in
  install-url)
    state="agentium-slack-$(date +%s)"
    scopes="$default_scopes"
    user_scopes="$default_user_scopes"
    while [ $# -gt 0 ]; do
      case "$1" in
        --state)
          state="${2:-}"
          shift 2
          ;;
        --scopes)
          scopes="${2:-}"
          shift 2
          ;;
        --user-scopes)
          user_scopes="${2:-}"
          shift 2
          ;;
        *)
          echo "Unknown argument for install-url: $1" >&2
          usage
          exit 1
          ;;
      esac
    done

    if [ -z "$SLACK_APP_CLIENT_ID" ] || [ -z "$SLACK_REDIRECT_URI" ]; then
      echo "SLACK_APP_CLIENT_ID and SLACK_REDIRECT_URI are required." >&2
      exit 1
    fi

    install_url="${SLACK_OAUTH_BASE_URL}/oauth/v2/authorize?client_id=$(urlencode "$SLACK_APP_CLIENT_ID")&redirect_uri=$(urlencode "$SLACK_REDIRECT_URI")&scope=$(urlencode "$scopes")&user_scope=$(urlencode "$user_scopes")&state=$(urlencode "$state")"

    cat <<EOF
Open this install URL in your browser:

$install_url

After approval Slack redirects to:
  $SLACK_REDIRECT_URI?code=...&state=$state

Then run:
  scripts/slack-oauth-helper.sh exchange-code --code "<code>"
EOF
    ;;

  exchange-code)
    code=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --code)
          code="${2:-}"
          shift 2
          ;;
        *)
          echo "Unknown argument for exchange-code: $1" >&2
          usage
          exit 1
          ;;
      esac
    done

    if [ -z "$code" ]; then
      echo "--code is required for exchange-code." >&2
      exit 1
    fi
    if [ -z "$SLACK_APP_CLIENT_ID" ] || [ -z "$SLACK_APP_CLIENT_SECRET" ] || [ -z "$SLACK_REDIRECT_URI" ]; then
      echo "SLACK_APP_CLIENT_ID, SLACK_APP_CLIENT_SECRET, and SLACK_REDIRECT_URI are required." >&2
      exit 1
    fi

    token_endpoint="${SLACK_OAUTH_BASE_URL}/api/oauth.v2.access"
    response="$(
      curl -sS -X POST "$token_endpoint" \
        -H "Content-Type: application/x-www-form-urlencoded" \
        --data-urlencode "code=$code" \
        --data-urlencode "client_id=$SLACK_APP_CLIENT_ID" \
        --data-urlencode "client_secret=$SLACK_APP_CLIENT_SECRET" \
        --data-urlencode "redirect_uri=$SLACK_REDIRECT_URI"
    )"

    ok="$(echo "$response" | jq -r '.ok // false')"
    if [ "$ok" != "true" ]; then
      echo "OAuth exchange failed:" >&2
      echo "$response" | jq -C . >&2 || echo "$response" >&2
      exit 1
    fi

    bot_token="$(echo "$response" | jq -r '.access_token // empty')"
    user_token="$(echo "$response" | jq -r '.authed_user.access_token // empty')"
    team_id="$(echo "$response" | jq -r '.team.id // empty')"
    enterprise_id="$(echo "$response" | jq -r '.enterprise.id // empty')"

    cat <<EOF
OAuth exchange succeeded.
Add these to your shell/.env (do not commit secrets):

export SLACK_APP_CLIENT_ID="$(echo "$SLACK_APP_CLIENT_ID")"
export SLACK_REDIRECT_URI="$(echo "$SLACK_REDIRECT_URI")"
EOF
    if [ -n "$bot_token" ]; then
      echo "export SLACK_BOT_TOKEN=\"$bot_token\""
    fi
    if [ -n "$user_token" ]; then
      echo "export SLACK_USER_TOKEN=\"$user_token\""
    fi
    if [ -n "$team_id" ]; then
      echo "export SLACK_TEAM_ID=\"$team_id\""
    fi
    if [ -n "$enterprise_id" ]; then
      echo "export SLACK_ENTERPRISE_ID=\"$enterprise_id\""
    fi
    ;;

  *)
    echo "Unknown command: $command" >&2
    usage
    exit 1
    ;;
esac
