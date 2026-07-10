#!/bin/sh
# timelog.sh — append-only activity time logging for the `bet` project.
#
# One file per agent-session under timelog/events/, named
#   <UTC-compact>__<task-slug>__<uuid>.jsonl
# The uuid guarantees no two agents ever write the same file, so parallel
# agents never overwrite each other and committed logs never merge-conflict.
#
# Usage:
#   scripts/timelog.sh in <activity> --task <slug> [--note "..."] [--agent NAME]
#       -> creates the logfile, writes the "in" event, PRINTS THE PATH (remember it)
#   scripts/timelog.sh switch <activity> --file <path> [--note "..."] [--agent NAME]
#   scripts/timelog.sh out            --file <path> [--note "..."] [--agent NAME]
#   scripts/timelog.sh status
#
# Activities (fixed enum): planning writing testing reviewing debugging docs research ci other
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
EVENTS="$ROOT/timelog/events"
ACTIVITIES="planning writing testing reviewing debugging docs research ci other"

now()  { date -u +%Y-%m-%dT%H:%M:%SZ; }
die()  { echo "timelog: $*" >&2; exit 1; }

json_escape() {
  # escape backslash + double-quote, drop control chars/newlines
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g' | tr -d '\n\r\t'
}

validate_activity() {
  for a in $ACTIVITIES; do [ "$a" = "$1" ] && return 0; done
  die "unknown activity '$1' (choose: $ACTIVITIES)"
}

cmd=${1:-help}
[ $# -gt 0 ] && shift || true

activity=""; task=""; file=""; note=""; agent="${TIMELOG_AGENT:-agent}"
while [ $# -gt 0 ]; do
  case "$1" in
    --task)  task=${2:-};  shift 2 ;;
    --file)  file=${2:-};  shift 2 ;;
    --note)  note=${2:-};  shift 2 ;;
    --agent) agent=${2:-}; shift 2 ;;
    --*)     die "unknown flag: $1" ;;
    *)       if [ -z "$activity" ]; then activity=$1; shift; else die "unexpected arg: $1"; fi ;;
  esac
done

case "$cmd" in
  in)
    [ -n "$activity" ] || die "usage: timelog.sh in <activity> --task <slug> [--note ..]"
    [ -n "$task" ]     || die "'in' requires --task <slug> (see timelog/tasks.toml)"
    # $task is interpolated into the logfile path below, so it must be a bare
    # tasks.toml slug — never a path. Enforce ^[A-Za-z0-9._-]+$ and reject the
    # pure-dot traversal names (which the char-class alone would let through).
    case "$task" in
      *[!A-Za-z0-9._-]*) die "invalid --task '$task': slug must match [A-Za-z0-9._-]+ (a timelog/tasks.toml name, not a path)" ;;
      .|..)              die "invalid --task '$task': slug must be a timelog/tasks.toml name, not a path" ;;
    esac
    validate_activity "$activity"
    mkdir -p "$EVENTS"
    uuid=$(uuidgen | tr 'A-Z' 'a-z')
    ts=$(now)
    stamp=$(printf '%s' "$ts" | tr -d ':-')
    file="$EVENTS/${stamp}__${task}__${uuid}.jsonl"
    printf '{"ts":"%s","agent":"%s","task":"%s","event":"in","activity":"%s","note":"%s"}\n' \
      "$ts" "$(json_escape "$agent")" "$(json_escape "$task")" "$activity" "$(json_escape "$note")" >> "$file"
    echo "$file"
    ;;
  switch)
    [ -n "$activity" ] || die "usage: timelog.sh switch <activity> --file <path> [--note ..]"
    [ -n "$file" ]     || die "'switch' requires --file <path> (printed by 'in')"
    [ -f "$file" ]     || die "no such logfile: $file"
    validate_activity "$activity"
    printf '{"ts":"%s","agent":"%s","event":"switch","activity":"%s","note":"%s"}\n' \
      "$(now)" "$(json_escape "$agent")" "$activity" "$(json_escape "$note")" >> "$file"
    echo "switched to $activity"
    ;;
  out)
    [ -n "$file" ] || die "'out' requires --file <path>"
    [ -f "$file" ] || die "no such logfile: $file"
    printf '{"ts":"%s","agent":"%s","event":"out","activity":"%s","note":"%s"}\n' \
      "$(now)" "$(json_escape "$agent")" "${activity:-}" "$(json_escape "$note")" >> "$file"
    echo "clocked out"
    ;;
  status)
    if [ -d "$EVENTS" ] && [ -n "$(ls -A "$EVENTS" 2>/dev/null)" ]; then ls -1 "$EVENTS"; else echo "(no events yet)"; fi
    ;;
  *)
    cat <<EOF
timelog.sh — append-only activity logging (one file per agent-session)
  in <activity> --task <slug> [--note ..] [--agent NAME]   -> prints the logfile path
  switch <activity> --file <path> [--note ..]
  out --file <path> [--note ..]
  status
activities: $ACTIVITIES
EOF
    ;;
esac
