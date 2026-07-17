# Time & velocity tracking — CLOSED historical record

**This system is retired. Do not log time. Nothing new gets written here.**

During the bootstrap, every agent working on `bet` logged its active effort —
planning, writing, testing, etc. — and that fed a velocity-based ETA. The data
is kept: **714h 47m of active effort across 210 event files**, covering the run
from the empty skeleton to a self-hosting fixpoint and real DOOM. It is a decent
answer to "what did this actually cost", and it is worth keeping for that alone.

It is no longer worth its price. The write side cost the repo a `PostToolUse` hook
that appended a heartbeat after **every tool call** (so `git status` was never
clean) and 47 of 387 commits — 12% of the history — that were pure `chore(timelog)`
clock-punching. Meanwhile `cargo xtask timelog eta`, the report all of that fed,
sat broken for 11 days behind a duplicate-key parse error in `tasks.toml` that
nobody noticed, because nothing in CI ever ran the parser. Every agent kept
dutifully logging into a report that could not be computed. That settled it.

Retired in #93. Track live work in GitHub issues instead.

## What's still here

The **read side** works, against the frozen data:

```sh
cargo xtask timelog report          # per-activity + per-task active totals, grand total
cargo xtask timelog eta             # velocity + ETA, as of the freeze
cargo xtask timelog report --json   # machine-readable
```

`scripts/timelog.sh` and `scripts/timelog-hook.sh` remain on disk but should not
be run. The `PostToolUse` hook that invoked the latter is gone from
`.claude/settings.json` — please don't re-add it.

## Files

```
timelog/
├── README.md          # this file
├── tasks.toml         # work breakdown: slug, name, size(points), status — frozen
└── events/            # append-only logs, frozen
    ├── <UTC>__<task-slug>__<uuid>.jsonl   # SPAN log — one per agent-session (semantic)
    └── <YYYYMMDD>__auto-<session_id>.jsonl# HOOK log — mechanical heartbeats (per session/day)
```

Every writer touched only its own UUID- or session-named file, so no two writers
ever shared a file and the committed logs never merge-conflicted.

## Event schema (JSONL, one object per line)

Retained so the archived data stays readable.

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
the last heartbeat of the same session from the hook log; 23 spans ended that way.
