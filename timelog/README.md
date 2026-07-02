# Time & velocity tracking

This directory records **active build effort** on `bet` — planning, writing, testing,
etc. — across every agent that ever works on the repo, and drives a velocity-based
ETA. It is committed so the totals are durable and aggregate across machines and
sessions.

## Why it never conflicts

Every writer touches **only its own file**, named with a UUID (span logs) or a
session id (hook logs). No two writers share a file, so:

- parallel agents never overwrite each other,
- no locks or coordination are needed,
- committed logs never merge-conflict (the filenames differ).

The reader (`cargo xtask timelog report`) is read-only.

## Files

```
timelog/
├── README.md          # this file
├── tasks.toml         # work breakdown: slug, name, size(points), status — drives ETA
└── events/            # append-only logs (git-tracked)
    ├── <UTC>__<task-slug>__<uuid>.jsonl   # SPAN log — one per agent-session (semantic)
    └── <YYYYMMDD>__auto-<session_id>.jsonl# HOOK log — mechanical heartbeats (per session/day)
```

## Event schema (JSONL, one object per line)

Span events (written by `scripts/timelog.sh`):

```json
{"ts":"2026-07-02T15:40:03Z","agent":"root","task":"step0-skeleton","event":"in","activity":"planning","note":"scoping"}
{"ts":"2026-07-02T15:52:10Z","agent":"root","event":"switch","activity":"writing","note":"manifests"}
{"ts":"2026-07-02T16:20:00Z","agent":"root","event":"out","activity":"writing","note":""}
```

Hook events (written by `scripts/timelog-hook.sh`, no activity label — heartbeats):

```json
{"ts":"2026-07-02T15:41:07Z","event":"tool","tool":"Edit","session":"abc123"}
```

Activities (fixed enum): `planning writing testing reviewing debugging docs research ci other`.

## How duration is computed

Within one span file, sort events by `ts`; each `in`/`switch` opens a span for its
activity that ends at the next event's `ts` (or at `out`). **Any gap longer than the
idle cap (default 5 min) is clamped to zero**, so idle/waiting time is not counted —
totals reflect real effort, not wall-clock. A span left open (no `out`) is closed at
the last heartbeat of the same session from the hook log.

## Commands

```sh
# clock in / switch activity / clock out (fast, no build needed):
scripts/timelog.sh in writing --task step0-skeleton --note "cargo manifests"   # prints the logfile path
scripts/timelog.sh switch testing --file <path printed above>
scripts/timelog.sh out --file <path>

# analyze (needs the workspace built):
cargo xtask timelog report          # per-activity + per-task active totals, grand total
cargo xtask timelog eta             # velocity + estimated time to completion
cargo xtask timelog report --json   # machine-readable
```

## The rule

Every agent (root and subagents) must clock in when it starts, switch when the
activity changes, and clock out when it pauses/finishes — see the "Time tracking"
section of the repo-root `CLAUDE.md`. The PostToolUse hook in `.claude/settings.json`
is the automatic backstop.
