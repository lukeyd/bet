#!/bin/sh
# timelog-hook.sh — PostToolUse heartbeat for the `bet` time-tracking system.
#
# Configured in .claude/settings.json to fire after every tool call. It appends
# ONE timestamped line to a per-session file and exits. It must stay near-instant:
# NO cargo, NO network, NO heavy parsing. It is mechanical ground truth that the
# xtask timelog report uses to (a) close spans an agent forgot to clock out and
# (b) flag "drift" (a span claiming time with no tool activity behind it).
#
# Per-session filename is stable across a day so repeated calls append to one file:
#   timelog/events/<YYYYMMDD>__auto-<session_id>.jsonl
set -u

ROOT="${CLAUDE_PROJECT_DIR:-}"
if [ -z "$ROOT" ]; then
  SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
  ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
fi
EVENTS="$ROOT/timelog/events"

json_escape() {
  # escape backslash + double-quote, drop control chars/newlines
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g' | tr -d '\n\r\t'
}

payload=$(cat 2>/dev/null || true)
sid=$(printf '%s' "$payload"  | sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
tool=$(printf '%s' "$payload" | sed -n 's/.*"tool_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
# $sid is interpolated into the logfile path below, so it must be a bare token —
# never a path. Enforce ^[A-Za-z0-9_-]+$; anything else (incl. empty, '/', '..')
# falls back to a safe literal so the heartbeat can never escape timelog/events/.
case "$sid" in
  ""|*[!A-Za-z0-9_-]*) sid=unknown ;;
esac
[ -n "$tool" ] || tool=unknown

mkdir -p "$EVENTS" 2>/dev/null || exit 0
ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
day=$(date -u +%Y%m%d)
printf '{"ts":"%s","event":"tool","tool":"%s","session":"%s"}\n' "$ts" "$(json_escape "$tool")" "$sid" \
  >> "$EVENTS/${day}__auto-${sid}.jsonl" 2>/dev/null || true

exit 0
