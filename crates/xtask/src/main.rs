//! `xtask` — repo automation for the `bet` project, run as `cargo xtask <command>`.
//!
//! Commands:
//!   graph-check         enforce the internal dependency graph (graph-allowlist.toml)
//!   timelog report      per-activity / per-task active-time totals from timelog/events
//!   timelog eta         velocity + estimated time to completion (timelog/tasks.toml)
//!   setup-llvm          print per-OS guidance for the pinned LLVM (backend --features llvm)
//!   corpus --check      structural lint of tests/corpus (pairing + feature coverage)
//!   corpus              execute each program via `bet run` and diff stdout vs .expected
//!   corpus --compiled   compiled differential column: `bet build` + run each program and
//!                       assert its stdout == .expected == the interpreter's stdout
//!   dist                stub (lands with release work)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

const USAGE: &str = "\
usage: cargo xtask <command>
  graph-check                 verify workspace deps against graph-allowlist.toml
  timelog report [--json] [--idle-cap SECS]
  timelog eta    [--hours-per-day N] [--idle-cap SECS]
  setup-llvm                  per-OS LLVM install guidance
  corpus [--check|--compiled] --check:    structural lint of tests/corpus;
                              --compiled: compiled differential column (needs an LLVM
                                          codegen driver; skipped cleanly without one);
                              else:       execute each program via `bet run` and diff
                                          stdout vs its .expected
  selfhost                    self-hosted-frontend tracer: build selfhost/betfe.bet, run it to
                                          emit .mir, compile+run that, assert the hello oracle
                                          (needs an LLVM codegen driver; skipped cleanly without one)
  dist                        (stub — release packaging)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let rest: &[String] = if args.len() > 1 { &args[1..] } else { &[] };

    let res = match cmd {
        "graph-check" => graph_check(),
        "timelog" => timelog(rest),
        "setup-llvm" => setup_llvm(),
        "corpus" => corpus(rest),
        "selfhost" => selfhost(&workspace_root()),
        "dist" => stub_cmd("dist", "release-artifact packaging for the 6 targets"),
        "" => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
        other => {
            eprintln!("xtask: unknown command {other:?}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// `crates/xtask/` -> repo root is two parents up.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn stub_cmd(name: &str, what: &str) -> Result<()> {
    println!("xtask {name}: not implemented yet — lands with {what}.");
    Ok(())
}

// ---------------------------------------------------------------------------
// graph-check
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct AllowFile {
    allow: BTreeMap<String, Vec<String>>,
}

fn graph_check() -> Result<()> {
    use cargo_metadata::{DependencyKind, MetadataCommand};

    let root = workspace_root();

    let allow_path = root.join("graph-allowlist.toml");
    let allow_src = std::fs::read_to_string(&allow_path)
        .with_context(|| format!("reading {}", allow_path.display()))?;
    let allow: AllowFile = toml::from_str(&allow_src).context("parsing graph-allowlist.toml")?;

    let metadata = MetadataCommand::new()
        .manifest_path(root.join("Cargo.toml"))
        .exec()
        .context("running `cargo metadata`")?;

    let members: std::collections::BTreeSet<String> = metadata
        .workspace_packages()
        .iter()
        .map(|p| p.name.to_string())
        .collect();

    let mut violations: Vec<String> = Vec::new();

    for pkg in metadata.workspace_packages() {
        let name = pkg.name.to_string();
        let allowed: std::collections::BTreeSet<&String> = allow
            .allow
            .get(&name)
            .map(|v| v.iter().collect())
            .unwrap_or_default();

        if !allow.allow.contains_key(&name) {
            violations.push(format!(
                "  crate `{name}` has no entry in graph-allowlist.toml"
            ));
        }

        for dep in &pkg.dependencies {
            if dep.kind == DependencyKind::Development {
                continue; // dev-deps (test helpers) may cross freely
            }
            let dname = dep.name.to_string();
            if dname == name || !members.contains(&dname) {
                continue; // self or external crate — not our concern
            }
            if !allowed.contains(&dname) {
                violations.push(format!("  {name} -> {dname}  (edge not in allowlist)"));
            }
        }
    }

    // Hygiene: warn (don't fail) on allowlist entries for crates that no longer exist.
    for k in allow.allow.keys() {
        if !members.contains(k) {
            eprintln!("warning: allowlist entry `{k}` is not a workspace crate");
        }
    }

    if violations.is_empty() {
        println!("graph-check OK ({} crates)", members.len());
        Ok(())
    } else {
        violations.sort();
        bail!("dependency-graph violations:\n{}", violations.join("\n"));
    }
}

// ---------------------------------------------------------------------------
// timelog
// ---------------------------------------------------------------------------

const DEFAULT_IDLE_CAP_SECS: i64 = 300; // gaps longer than this are clamped (idle)

#[derive(serde::Deserialize, Default)]
struct RawEvent {
    ts: String,
    #[serde(default)]
    event: String,
    #[serde(default)]
    activity: String,
    #[serde(default)]
    session: String,
}

struct SpanEvent {
    secs: i64,
    event: String,
    activity: String,
}

struct SpanFile {
    task: String,
    session: String,
    events: Vec<SpanEvent>,
}

/// A heartbeat: (epoch seconds, session id).
type Beat = (i64, String);
/// Parsed contents of `timelog/events/`: semantic span files + mechanical heartbeats.
type LoadedEvents = (Vec<SpanFile>, Vec<Beat>);

#[derive(Default)]
struct Totals {
    per_activity: BTreeMap<String, i64>,
    per_task: BTreeMap<String, i64>,
    grand: i64,
    unclosed: usize,
}

fn timelog(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("report");
    let flags: &[String] = if args.len() > 1 { &args[1..] } else { &[] };
    let idle_cap = flag_value(flags, "--idle-cap")
        .map(|v| v.parse::<i64>())
        .transpose()
        .context("--idle-cap must be an integer number of seconds")?
        .unwrap_or(DEFAULT_IDLE_CAP_SECS);

    let root = workspace_root();
    let (spans, beats) = load_events(&root.join("timelog").join("events"))?;
    let totals = compute_totals(&spans, &beats, idle_cap);

    match sub {
        "report" => {
            if flags.iter().any(|f| f == "--json") {
                report_json(&totals);
            } else {
                report_human(&totals, idle_cap);
            }
            Ok(())
        }
        "eta" => {
            let hpd = flag_value(flags, "--hours-per-day")
                .map(|v| v.parse::<f64>())
                .transpose()
                .context("--hours-per-day must be a number")?;
            eta(&root, &totals, hpd)
        }
        other => bail!("unknown `timelog` subcommand {other:?} (use report|eta)"),
    }
}

fn flag_value<'a>(flags: &'a [String], name: &str) -> Option<&'a str> {
    flags
        .iter()
        .position(|f| f == name)
        .and_then(|i| flags.get(i + 1))
        .map(String::as_str)
}

/// Parse every `*.jsonl` under `events/`. Files whose name contains `__auto-` are
/// heartbeat logs (mechanical); the rest are span logs (semantic, task from filename).
fn load_events(dir: &Path) -> Result<LoadedEvents> {
    let mut spans = Vec::new();
    let mut beats: Vec<Beat> = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok((spans, beats)), // no events yet
    };

    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;

        if stem.contains("__auto-") {
            let sess = stem.rsplit("auto-").next().unwrap_or("").to_string();
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                if let Ok(ev) = serde_json::from_str::<RawEvent>(line)
                    && let Some(secs) = parse_ts(&ev.ts)
                {
                    let s = if ev.session.is_empty() {
                        sess.clone()
                    } else {
                        ev.session
                    };
                    beats.push((secs, s));
                }
            }
        } else {
            let task = task_from_stem(stem);
            let mut events = Vec::new();
            let mut session = String::new();
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                if let Ok(ev) = serde_json::from_str::<RawEvent>(line)
                    && let Some(secs) = parse_ts(&ev.ts)
                {
                    if session.is_empty() && !ev.session.is_empty() {
                        session = ev.session.clone();
                    }
                    events.push(SpanEvent {
                        secs,
                        event: ev.event,
                        activity: ev.activity,
                    });
                }
            }
            if !events.is_empty() {
                events.sort_by_key(|e| e.secs);
                spans.push(SpanFile {
                    task,
                    session,
                    events,
                });
            }
        }
    }
    Ok((spans, beats))
}

/// Extract the task slug from `<stamp>__<task>__<uuid>` (task may itself contain `__`).
fn task_from_stem(stem: &str) -> String {
    let parts: Vec<&str> = stem.split("__").collect();
    if parts.len() >= 3 {
        parts[1..parts.len() - 1].join("__")
    } else {
        "unknown".to_string()
    }
}

fn compute_totals(spans: &[SpanFile], beats: &[Beat], idle_cap: i64) -> Totals {
    let mut t = Totals::default();

    for span in spans {
        let first = span.events.first().map(|e| e.secs).unwrap_or(0);
        let last = span.events.last().map(|e| e.secs).unwrap_or(0);
        let has_out = span.events.last().map(|e| e.event.as_str()) == Some("out");

        // Match heartbeats to this span: by session if we have one that matches,
        // otherwise by time window (correct for a single concurrent agent).
        let session_match =
            !span.session.is_empty() && beats.iter().any(|(_, s)| *s == span.session);
        let mut matched: Vec<i64> = beats
            .iter()
            .filter(|(bs, s)| {
                *bs >= first
                    && if session_match {
                        *s == span.session
                    } else {
                        true
                    }
            })
            .map(|(bs, _)| *bs)
            .collect();

        let end = if has_out {
            matched.retain(|b| *b <= last);
            last
        } else {
            let max_beat = matched.iter().copied().max().unwrap_or(last);
            t.unclosed += 1;
            max_beat.max(last)
        };

        // Densified, de-duplicated timeline within [first, end].
        let mut points: Vec<i64> = span
            .events
            .iter()
            .map(|e| e.secs)
            .chain(matched)
            .filter(|p| *p >= first && *p <= end)
            .collect();
        points.push(end);
        points.sort_unstable();
        points.dedup();

        for pair in points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let activity = current_activity(&span.events, a);
            let Some(activity) = activity else { continue };
            let add = (b - a).clamp(0, idle_cap);
            if add == 0 {
                continue;
            }
            *t.per_activity.entry(activity.to_string()).or_insert(0) += add;
            *t.per_task.entry(span.task.clone()).or_insert(0) += add;
            t.grand += add;
        }
    }
    t
}

/// Activity in effect at time `at`: the most recent `in`/`switch` event at or before it.
fn current_activity(events: &[SpanEvent], at: i64) -> Option<&str> {
    events
        .iter()
        .rfind(|e| e.secs <= at && (e.event == "in" || e.event == "switch"))
        .map(|e| e.activity.as_str())
}

fn report_human(t: &Totals, idle_cap: i64) {
    println!(
        "bet — active time (idle gaps > {}s not counted)\n",
        idle_cap
    );
    if t.grand == 0 {
        println!("  (no time logged yet)");
        return;
    }
    println!("  by activity:");
    for (k, v) in &t.per_activity {
        println!("    {:<12} {}", k, fmt_hms(*v));
    }
    println!("\n  by task:");
    for (k, v) in &t.per_task {
        println!("    {:<20} {}", k, fmt_hms(*v));
    }
    println!("\n  total active: {}", fmt_hms(t.grand));
    if t.unclosed > 0 {
        println!(
            "  note: {} span(s) never clocked out (closed at last heartbeat)",
            t.unclosed
        );
    }
}

fn report_json(t: &Totals) {
    let activities: BTreeMap<&String, i64> = t.per_activity.iter().map(|(k, v)| (k, *v)).collect();
    let tasks: BTreeMap<&String, i64> = t.per_task.iter().map(|(k, v)| (k, *v)).collect();
    let out = serde_json::json!({
        "seconds_by_activity": activities,
        "seconds_by_task": tasks,
        "total_active_seconds": t.grand,
        "unclosed_spans": t.unclosed,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}

// ---------------------------------------------------------------------------
// eta
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct TasksFile {
    #[serde(default)]
    task: Vec<TaskDef>,
}

#[derive(serde::Deserialize)]
struct TaskDef {
    slug: String,
    size: f64,
    status: String,
}

fn eta(root: &Path, totals: &Totals, hours_per_day: Option<f64>) -> Result<()> {
    let path = root.join("timelog").join("tasks.toml");
    let src =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let tasks: TasksFile = toml::from_str(&src).context("parsing timelog/tasks.toml")?;

    let mut done_points = 0.0;
    let mut done_secs: i64 = 0;
    let mut remaining_points = 0.0;
    let mut doing_secs: i64 = 0;

    for t in &tasks.task {
        let secs = totals.per_task.get(&t.slug).copied().unwrap_or(0);
        match t.status.as_str() {
            "done" => {
                done_points += t.size;
                done_secs += secs;
            }
            "doing" => {
                remaining_points += t.size;
                doing_secs += secs;
            }
            _ => remaining_points += t.size,
        }
    }

    println!("bet — velocity & ETA\n");
    println!("  total active logged: {}", fmt_hms(totals.grand));
    println!("  points done: {done_points:.0}   remaining: {remaining_points:.0}");
    if doing_secs > 0 {
        println!(
            "  (already spent on in-progress tasks: {})",
            fmt_hms(doing_secs)
        );
    }

    if done_points == 0.0 || done_secs == 0 {
        println!(
            "\n  ETA: insufficient data — need at least one task marked status=\"done\"\n\
             \x20 with logged time. Mark the current task done in timelog/tasks.toml once it lands."
        );
        return Ok(());
    }

    let done_hours = done_secs as f64 / 3600.0;
    let velocity = done_points / done_hours; // points per active-hour
    let eta_hours = remaining_points / velocity;

    println!("\n  velocity: {velocity:.2} points / active-hour");
    println!("  ETA (remaining): {eta_hours:.1} active-hours");
    if let Some(hpd) = hours_per_day
        && hpd > 0.0
    {
        println!(
            "  ETA (calendar @ {hpd:.1} active-h/day): {:.1} days",
            eta_hours / hpd
        );
    }
    println!(
        "\n  (in-progress tasks count their full size toward remaining; sizes are\n\
         \x20 estimates, so early ETAs are wide and tighten as more tasks close.)"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// setup-llvm
// ---------------------------------------------------------------------------

fn setup_llvm() -> Result<()> {
    const LLVM: &str = "18";
    println!(
        "\
bet pins LLVM {LLVM}. Building `backend` with `--features llvm` requires it installed:
  macOS:   brew install llvm@{LLVM}     (export LLVM_SYS_181_PREFIX=\"$(brew --prefix llvm@{LLVM})\")
  Ubuntu:  sudo apt-get install -y llvm-{LLVM}-dev libpolly-{LLVM}-dev
  Windows: install the LLVM {LLVM} release, then set LLVM_SYS_181_PREFIX

Step 0 does NOT build LLVM — the default workspace build has no LLVM dependency.
This command is guidance-only until the backend work begins."
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

/// Parse an RFC3339 UTC timestamp like `2026-07-02T21:06:47Z` into epoch seconds.
fn parse_ts(s: &str) -> Option<i64> {
    let s = s.trim();
    let s = s.strip_suffix('Z').unwrap_or(s);
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: i64 = d.next()?.parse().ok()?;
    let da: i64 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next()?.parse().ok()?;
    let se: i64 = t.next().unwrap_or("0").parse().ok()?;
    Some(days_from_civil(y, mo, da) * 86_400 + h * 3_600 + mi * 60 + se)
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn fmt_hms(secs: i64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

// ---------------------------------------------------------------------------
// corpus — `--check` is the structural lint of tests/corpus (Step 1c); the bare
// command executes each program on the interpreter (`bet run`) and diffs stdout
// against its `.expected`, gated by the manifest's per-program `interp` field.
// `--compiled` is the other half of the differential runner: it `bet build`s each
// opted-in program to a native executable and asserts stdout == .expected == interp
// (the interp == compiled invariant), gated by the per-program `compiled` field.
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct CorpusManifest {
    features: CorpusFeatures,
    #[serde(default)]
    program: Vec<CorpusProgram>,
}

#[derive(serde::Deserialize)]
struct CorpusFeatures {
    all: Vec<String>,
}

#[derive(serde::Deserialize)]
struct CorpusProgram {
    path: String,
    #[serde(default)]
    covers: Vec<String>,
    /// How the interpreter (`bet run`) is expected to handle this program under the execute
    /// runner (`cargo xtask corpus`). Defaults to `pass`.
    #[serde(default)]
    interp: InterpMode,
    /// How the native code generator (`bet build` + running the executable) is expected to
    /// handle this program under the compiled differential runner (`cargo xtask corpus
    /// --compiled`). Defaults to `skip` — a program opts in to the compiled gate explicitly.
    #[serde(default)]
    compiled: CompiledMode,
}

/// The per-program interpreter expectation gated by `cargo xtask corpus`.
#[derive(serde::Deserialize, Default, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
enum InterpMode {
    /// Must run and match `.expected` byte-for-byte (the gated set).
    #[default]
    Pass,
    /// Must still error (non-zero `bet run`); a newly-passing one is flagged (ratchet).
    Unsupported,
    /// Not executed at all (parses, but out of the interpreter's scope for now).
    Skip,
}

/// The per-program compiled-codegen expectation gated by `cargo xtask corpus --compiled`.
/// Only two states (no `unsupported` ratchet): a program is either held to the full
/// differential invariant or left out of the compiled column entirely.
#[derive(serde::Deserialize, Default, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
enum CompiledMode {
    /// Must `bet build` to a native executable, run cleanly, and produce stdout that equals
    /// `.expected` byte-for-byte AND equals the interpreter's stdout for the same program
    /// (interp == compiled — the differential invariant). A failure here is a hard error.
    Pass,
    /// Not compiled or run. The default: codegen for this program is out of scope until the
    /// backend/runtime grow to cover it (then flip it to `pass`).
    #[default]
    Skip,
}

fn corpus(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a.as_str() == "--check") {
        corpus_check(&workspace_root())
    } else if args.iter().any(|a| a.as_str() == "--compiled") {
        // `--real-runtime` links each compiled program against the real `runtime` archive
        // instead of the headless `rt-stub` — the same differential, run against production
        // runtime code.
        let real = args.iter().any(|a| a.as_str() == "--real-runtime");
        corpus_compiled(&workspace_root(), real)
    } else {
        corpus_run(&workspace_root())
    }
}

/// Execute every corpus program through the interpreter (`bet run`) and diff stdout against its
/// `.expected`, gated by each program's `interp` field:
///   * `pass`        — must run and match `.expected` byte-for-byte;
///   * `unsupported` — must still error (non-zero exit), or it is flagged for promotion;
///   * `skip`        — not run.
///
/// Fails (non-zero) if any `pass` program regresses or any `unsupported` program starts passing.
fn corpus_run(root: &Path) -> Result<()> {
    let dir = root.join("tests").join("corpus");
    let manifest_path = dir.join("MANIFEST.toml");
    let src = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest: CorpusManifest =
        toml::from_str(&src).context("parsing tests/corpus/MANIFEST.toml")?;

    // Build the driver once, then invoke the built binary per program (keeps `xtask = []`;
    // mirrors the `cargo metadata` shell-out that graph-check already relies on).
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = std::process::Command::new(&cargo)
        .args(["build", "-p", "driver"])
        .current_dir(root)
        .status()
        .context("running `cargo build -p driver`")?;
    if !status.success() {
        bail!("`cargo build -p driver` failed; cannot run the corpus");
    }
    let bin = root
        .join("target")
        .join("debug")
        .join(format!("bet{}", std::env::consts::EXE_SUFFIX));

    let mut want_pass = 0usize;
    let mut got_pass = 0usize;
    let mut unsupported = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for prog in &manifest.program {
        match prog.interp {
            InterpMode::Skip => skipped += 1,
            InterpMode::Pass => {
                want_pass += 1;
                let bet = dir.join(format!("{}.bet", prog.path));
                let expected_path = dir.join(format!("{}.expected", prog.path));
                let expected = std::fs::read(&expected_path)
                    .with_context(|| format!("reading {}", expected_path.display()))?;
                let out = std::process::Command::new(&bin)
                    .arg("run")
                    .arg(&bet)
                    .output()
                    .with_context(|| format!("running `bet run {}`", bet.display()))?;
                if out.status.success() && out.stdout == expected {
                    got_pass += 1;
                } else if !out.status.success() {
                    failures.push(format!(
                        "  FAIL {} — `bet run` errored (exit {}): {}",
                        prog.path,
                        out.status.code().unwrap_or(-1),
                        short(&out.stderr),
                    ));
                } else {
                    failures.push(format!(
                        "  FAIL {} — stdout mismatch\n         expected: {}\n         actual:   {}",
                        prog.path,
                        short(&expected),
                        short(&out.stdout),
                    ));
                }
            }
            InterpMode::Unsupported => {
                unsupported += 1;
                let bet = dir.join(format!("{}.bet", prog.path));
                let out = std::process::Command::new(&bin)
                    .arg("run")
                    .arg(&bet)
                    .output()
                    .with_context(|| format!("running `bet run {}`", bet.display()))?;
                // The ratchet: an `unsupported` program that now runs cleanly is coverage we
                // should lock in — flag it so the field gets flipped to `pass`.
                if out.status.success() {
                    failures.push(format!(
                        "  RATCHET {} — now runs without error; promote it to `interp = \"pass\"` \
                         (and verify its `.expected`)",
                        prog.path,
                    ));
                }
            }
        }
    }

    println!(
        "corpus: {}/{} pass  ({} unsupported, {} skip, {} total)",
        got_pass,
        want_pass,
        unsupported,
        skipped,
        manifest.program.len(),
    );
    if failures.is_empty() {
        Ok(())
    } else {
        failures.sort();
        bail!(
            "corpus run found {} problem(s):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

/// The compiled column of the differential runner (`cargo xtask corpus --compiled`). For each
/// `compiled = "pass"` program it `bet build`s a native executable, runs it, and asserts BOTH:
///   (a) its stdout equals `.expected` byte-for-byte, AND
///   (b) its stdout equals the interpreter's stdout for the same source
///       (interp == compiled — the differential invariant).
///
/// Requires a codegen-capable driver: `bet` built `--features llvm`, which needs LLVM 18 located
/// via `LLVM_SYS_180_PREFIX`. If that build fails or LLVM is not present, the WHOLE column is
/// skipped with a clear note and a zero exit — so environments without LLVM (the default CI
/// matrix included) are unaffected. When codegen IS available, any `compiled = "pass"` program
/// that fails to build/run/match is a hard error (non-zero exit) — that is the ratchet.
///
/// NOTE (Track X): on this branch only `01-basics/hello` and `01-basics/comments` are `pass` —
/// the `spill.it("literal")` tracer bullet, which compiles and runs end to end today. The
/// broader compiled `pass` set (computed-value / formatted printing, e.g. `spill.f`) is curated
/// by the orchestrator once Tracks R (rt-abi/runtime) and C (frontend/backend `spill` lowering)
/// merge — flip the relevant `compiled = "skip"` entries to `pass` at that integration point.
fn corpus_compiled(root: &Path, real_runtime: bool) -> Result<()> {
    let dir = root.join("tests").join("corpus");
    let manifest_path = dir.join("MANIFEST.toml");
    let src = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest: CorpusManifest =
        toml::from_str(&src).context("parsing tests/corpus/MANIFEST.toml")?;

    // Build the codegen-enabled driver once. If LLVM 18 isn't present (or the `--features llvm`
    // build otherwise fails), skip the whole compiled column gracefully with a zero exit: the
    // interp column and `--check` are the LLVM-free gates and must stay unaffected. We shell out
    // to the built binary per program (keeps `xtask = []` — no workspace dep on the driver).
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let built = std::process::Command::new(&cargo)
        .args(["build", "-p", "driver", "--features", "llvm"])
        .current_dir(root)
        .status()
        .context("running `cargo build -p driver --features llvm`")?;
    if !built.success() {
        println!(
            "compiled column skipped: no LLVM/codegen \
             (`cargo build -p driver --features llvm` failed — install LLVM 18 and set \
             LLVM_SYS_180_PREFIX to enable it). The interp column and `--check` are unaffected."
        );
        return Ok(());
    }

    // `bet build` links compiled programs against `librt_stub.a`, which cargo only stages next to
    // the `bet` binary when the `rt-stub` *package* is built (an rlib-only dep does not emit the
    // staticlib at the target root). Build it explicitly so the linker can find it.
    let staged = std::process::Command::new(&cargo)
        .args(["build", "-p", "rt-stub"])
        .current_dir(root)
        .status()
        .context("running `cargo build -p rt-stub` (stages librt_stub.a next to `bet`)")?;
    if !staged.success() {
        bail!("`cargo build -p rt-stub` failed; cannot link compiled corpus programs");
    }

    // For the real-runtime column, also stage `libruntime.a` next to `bet` (same rationale).
    if real_runtime {
        let staged_rt = std::process::Command::new(&cargo)
            .args(["build", "-p", "runtime"])
            .current_dir(root)
            .status()
            .context("running `cargo build -p runtime` (stages libruntime.a next to `bet`)")?;
        if !staged_rt.success() {
            bail!("`cargo build -p runtime` failed; cannot link against the real runtime");
        }
    }

    let bin = root
        .join("target")
        .join("debug")
        .join(format!("bet{}", std::env::consts::EXE_SUFFIX));

    // A private scratch dir for the compiled executables (and their intermediate `.o`s), removed
    // at the end. Namespaced by pid so concurrent runs never collide.
    let tmp = std::env::temp_dir().join(format!("bet-corpus-compiled-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)
        .with_context(|| format!("creating scratch dir {}", tmp.display()))?;

    let mut want_pass = 0usize;
    let mut got_pass = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for prog in &manifest.program {
        match prog.compiled {
            CompiledMode::Skip => skipped += 1,
            CompiledMode::Pass => {
                want_pass += 1;
                match run_compiled_program(&bin, &dir, &tmp, &prog.path, prog.interp, real_runtime)
                {
                    Ok(()) => got_pass += 1,
                    Err(msg) => failures.push(msg),
                }
            }
        }
    }

    let _ = std::fs::remove_dir_all(&tmp); // best-effort cleanup

    println!(
        "compiled: {}/{} pass  ({} skip, {} total)",
        got_pass,
        want_pass,
        skipped,
        manifest.program.len(),
    );
    if failures.is_empty() {
        Ok(())
    } else {
        failures.sort();
        bail!(
            "compiled column found {} problem(s):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

/// Build, run, and differentially check one `compiled = "pass"` program. Returns `Ok(())` when
/// the compiled executable's stdout equals `.expected` byte-for-byte AND — when the program's
/// `interp` field is `pass` — also equals the interpreter's stdout (the interp == compiled
/// invariant). For programs the interpreter can't run (`interp = unsupported`/`skip`, e.g. FFI or
/// width-carrying wrapping arithmetic) there is no interpreter output to diff against, so the
/// compiled gate is `.expected`-only. Otherwise a preformatted `FAIL ...` line for the first
/// failing stage.
fn run_compiled_program(
    bin: &Path,
    dir: &Path,
    tmp: &Path,
    stem: &str,
    interp_mode: InterpMode,
    real_runtime: bool,
) -> Result<(), String> {
    let bet = dir.join(format!("{stem}.bet"));
    let expected_path = dir.join(format!("{stem}.expected"));
    let expected = std::fs::read(&expected_path)
        .map_err(|e| format!("  FAIL {stem} — reading {}: {e}", expected_path.display()))?;

    // Stem can contain `/` (e.g. `01-basics/hello`); flatten it into a single scratch filename.
    let exe = tmp.join(format!(
        "{}{}",
        stem.replace('/', "_"),
        std::env::consts::EXE_SUFFIX
    ));

    // 1. Compile: `bet build <stem>.bet -o <exe> [--runtime real]`.
    let mut build_cmd = std::process::Command::new(bin);
    build_cmd.arg("build").arg(&bet).arg("-o").arg(&exe);
    if real_runtime {
        build_cmd.args(["--runtime", "real"]);
    }
    let build = build_cmd
        .output()
        .map_err(|e| format!("  FAIL {stem} — spawning `bet build`: {e}"))?;
    if !build.status.success() {
        return Err(format!(
            "  FAIL {stem} — `bet build` errored (exit {}): {}",
            build.status.code().unwrap_or(-1),
            short(&build.stderr),
        ));
    }

    // 2. Run the freshly linked native executable.
    let run = std::process::Command::new(&exe)
        .output()
        .map_err(|e| format!("  FAIL {stem} — spawning compiled `{}`: {e}", exe.display()))?;
    if !run.status.success() {
        return Err(format!(
            "  FAIL {stem} — compiled program exited non-zero (exit {}): {}",
            run.status.code().unwrap_or(-1),
            short(&run.stderr),
        ));
    }

    // 3a. Compiled stdout must equal the golden `.expected` byte-for-byte.
    if run.stdout != expected {
        return Err(format!(
            "  FAIL {stem} — compiled stdout != .expected\n\
             \x20        expected: {}\n         compiled: {}",
            short(&expected),
            short(&run.stdout),
        ));
    }

    // 3b. The differential invariant — only meaningful when the interpreter actually runs this
    // program (`interp = pass`). For `unsupported`/`skip` programs there is no interpreter output
    // to compare, so the compiled gate above (`.expected`) stands on its own.
    if matches!(interp_mode, InterpMode::Pass) {
        let interp = std::process::Command::new(bin)
            .arg("run")
            .arg(&bet)
            .output()
            .map_err(|e| format!("  FAIL {stem} — spawning `bet run` (interp): {e}"))?;
        if !interp.status.success() {
            return Err(format!(
                "  FAIL {stem} — interp `bet run` errored (exit {}): {}",
                interp.status.code().unwrap_or(-1),
                short(&interp.stderr),
            ));
        }
        if run.stdout != interp.stdout {
            return Err(format!(
                "  FAIL {stem} — interp != compiled (differential mismatch)\n\
                 \x20        interp:   {}\n         compiled: {}",
                short(&interp.stdout),
                short(&run.stdout),
            ));
        }
    }
    Ok(())
}

/// A compact, single-line rendering of captured output for a diff message: UTF-8 lossy with
/// escapes (so newlines show as `\n`), truncated so a long stream stays readable.
fn short(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let escaped: String = text.escape_debug().collect();
    // Truncate by chars (not bytes) so a multi-byte escape rendering never splits mid-char.
    if escaped.chars().count() > 200 {
        let head: String = escaped.chars().take(200).collect();
        format!("\"{head}…\"")
    } else {
        format!("\"{escaped}\"")
    }
}

/// Structural lint of `tests/corpus`: manifest <-> disk pairing and feature-coverage
/// completeness. Does not execute any program.
fn corpus_check(root: &Path) -> Result<()> {
    let dir = root.join("tests").join("corpus");
    let manifest_path = dir.join("MANIFEST.toml");
    let src = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest: CorpusManifest =
        toml::from_str(&src).context("parsing tests/corpus/MANIFEST.toml")?;

    let mut files = Vec::new();
    walk_files(&dir, &mut files)?;
    let bet = stems_with_ext(&files, &dir, "bet");
    let expected = stems_with_ext(&files, &dir, "expected");

    let problems = check_corpus(&manifest, &bet, &expected);
    if problems.is_empty() {
        println!(
            "corpus --check OK: {} programs, {} features all covered",
            manifest.program.len(),
            manifest.features.all.len()
        );
        Ok(())
    } else {
        bail!(
            "corpus --check found {} problem(s):\n{}",
            problems.len(),
            problems.join("\n")
        );
    }
}

/// Pure checker (no IO), so it is unit-testable. `bet`/`expected` are the program
/// stems (e.g. `01-basics/hello`) found on disk for each extension. Returns a sorted
/// list of problems; empty means the corpus is well-formed.
fn check_corpus(
    manifest: &CorpusManifest,
    bet: &std::collections::BTreeSet<String>,
    expected: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    use std::collections::BTreeSet;

    let mut problems: Vec<String> = Vec::new();

    // 1. Pairing: every .bet needs a .expected and vice-versa.
    for stem in bet.union(expected) {
        if !bet.contains(stem) {
            problems.push(format!("  {stem}.expected has no matching .bet"));
        }
        if !expected.contains(stem) {
            problems.push(format!("  {stem}.bet has no matching .expected"));
        }
    }
    let on_disk: BTreeSet<String> = bet.intersection(expected).cloned().collect();

    // 2. Feature universe + the listed programs.
    let all: BTreeSet<String> = manifest.features.all.iter().cloned().collect();
    let mut listed: BTreeSet<String> = BTreeSet::new();
    let mut covered: BTreeSet<String> = BTreeSet::new();

    for p in &manifest.program {
        if !listed.insert(p.path.clone()) {
            problems.push(format!(
                "  program `{}` is listed twice in MANIFEST",
                p.path
            ));
        }
        if !on_disk.contains(&p.path) {
            problems.push(format!(
                "  program `{}` is in MANIFEST but has no .bet/.expected pair on disk",
                p.path
            ));
        }
        for key in &p.covers {
            if !all.contains(key) {
                problems.push(format!(
                    "  program `{}` covers unknown feature `{key}` (not in [features].all)",
                    p.path
                ));
            }
            covered.insert(key.clone());
        }
    }

    // 3. Every on-disk pair must be registered in the manifest.
    for stem in &on_disk {
        if !listed.contains(stem) {
            problems.push(format!(
                "  {stem} exists on disk but is not listed in MANIFEST"
            ));
        }
    }

    // 4. Every declared feature must be covered by at least one program.
    for feat in &all {
        if !covered.contains(feat) {
            problems.push(format!("  feature `{feat}` is not covered by any program"));
        }
    }

    problems.sort();
    problems
}

/// Recursively collect every file under `dir`.
fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            walk_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// The set of program stems (path minus extension, `/`-separated, relative to `base`)
/// among `files` whose extension is `ext`.
fn stems_with_ext(files: &[PathBuf], base: &Path, ext: &str) -> std::collections::BTreeSet<String> {
    files
        .iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(ext))
        .filter_map(|p| p.strip_prefix(base).ok().map(|r| r.with_extension("")))
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .collect()
}

// ---------------------------------------------------------------------------
// selfhost — Phase-C tracer bullet
// ---------------------------------------------------------------------------

/// `cargo xtask selfhost` — prove the self-hosted-frontend pipeline end-to-end: build the
/// bet-written frontend `selfhost/betfe.bet` with the Rust compiler, run it to emit `.mir`,
/// compile that `.mir` with the backend, run the result, and assert it prints the `hello`
/// oracle's output. Also asserts betfe's `.mir` is byte-identical to the Rust frontend's
/// canonical dump — the strong-equivalence property Phase C must preserve.
///
/// Requires a codegen-capable driver (`--features llvm`); without LLVM the whole check is skipped
/// with a zero exit, exactly like `corpus --compiled`, so the LLVM-free gates are unaffected.
fn selfhost(root: &Path) -> Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let built = std::process::Command::new(&cargo)
        .args(["build", "-p", "driver", "--features", "llvm"])
        .current_dir(root)
        .status()
        .context("running `cargo build -p driver --features llvm`")?;
    if !built.success() {
        println!(
            "selfhost skipped: no LLVM/codegen \
             (`cargo build -p driver --features llvm` failed — install LLVM 18 and set \
             LLVM_SYS_180_PREFIX to enable it). The LLVM-free gates are unaffected."
        );
        return Ok(());
    }
    // `bet build` links against `librt_stub.a`, which cargo only stages next to `bet` when the
    // `rt-stub` package itself is built (mirrors `corpus --compiled`).
    let staged = std::process::Command::new(&cargo)
        .args(["build", "-p", "rt-stub"])
        .current_dir(root)
        .status()
        .context("running `cargo build -p rt-stub` (stages librt_stub.a next to `bet`)")?;
    if !staged.success() {
        bail!("`cargo build -p rt-stub` failed; cannot link the self-hosted program");
    }

    let bin = root
        .join("target")
        .join("debug")
        .join(format!("bet{}", std::env::consts::EXE_SUFFIX));
    let tmp = std::env::temp_dir().join(format!("bet-selfhost-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)
        .with_context(|| format!("creating scratch dir {}", tmp.display()))?;

    let result = selfhost_run(&bin, root, &tmp);
    let _ = std::fs::remove_dir_all(&tmp); // best-effort cleanup
    result
}

/// The staged pipeline behind `cargo xtask selfhost`, factored out so the scratch dir is always
/// cleaned up. Each stage bails with a focused message on failure.
fn selfhost_run(bin: &Path, root: &Path, tmp: &Path) -> Result<()> {
    let corpus = root.join("tests").join("corpus").join("01-basics");

    // 1. Build the bet-written frontend `betfe` with the Rust compiler.
    let betfe = tmp.join(format!("betfe{}", std::env::consts::EXE_SUFFIX));
    let build_betfe = std::process::Command::new(bin)
        .arg("build")
        .arg(root.join("selfhost").join("betfe.bet"))
        .arg("-o")
        .arg(&betfe)
        .output()
        .context("running `bet build selfhost/betfe.bet`")?;
    if !build_betfe.status.success() {
        bail!(
            "stage 1 (build betfe) failed:\n{}",
            String::from_utf8_lossy(&build_betfe.stderr)
        );
    }

    // 2. Run betfe to emit the `.mir` text.
    let emitted = std::process::Command::new(&betfe)
        .output()
        .context("running the betfe binary")?;
    if !emitted.status.success() {
        bail!(
            "stage 2 (run betfe) failed:\n{}",
            String::from_utf8_lossy(&emitted.stderr)
        );
    }
    let mir_path = tmp.join("hello.mir");
    std::fs::write(&mir_path, &emitted.stdout)
        .with_context(|| format!("writing {}", mir_path.display()))?;

    // 3. Compile the emitted `.mir` with the backend and 4. run it.
    let prog = tmp.join(format!("prog{}", std::env::consts::EXE_SUFFIX));
    let build_mir = std::process::Command::new(bin)
        .arg("build")
        .arg(&mir_path)
        .arg("-o")
        .arg(&prog)
        .output()
        .context("running `bet build hello.mir`")?;
    if !build_mir.status.success() {
        bail!(
            "stage 3 (compile betfe's .mir) failed:\n{}",
            String::from_utf8_lossy(&build_mir.stderr)
        );
    }
    let ran = std::process::Command::new(&prog)
        .output()
        .context("running the self-compiled program")?;
    let got = String::from_utf8_lossy(&ran.stdout).into_owned();

    // The `hello` oracle: the self-hosted pipeline must reproduce its expected output.
    let expected =
        std::fs::read_to_string(corpus.join("hello.expected")).context("reading hello.expected")?;
    if got != expected {
        bail!("stage 4 (run) output mismatch: expected {expected:?}, got {got:?}");
    }

    // Strong equivalence: betfe's `.mir` is byte-identical to the Rust frontend's canonical dump.
    let dump = std::process::Command::new(bin)
        .arg("build")
        .arg("--emit")
        .arg("mir")
        .arg(corpus.join("hello.bet"))
        .output()
        .context("running `bet build --emit mir hello.bet`")?;
    if !dump.status.success() {
        bail!(
            "emitting the reference .mir failed:\n{}",
            String::from_utf8_lossy(&dump.stderr)
        );
    }
    if emitted.stdout != dump.stdout {
        bail!(
            "betfe's .mir is not byte-identical to the Rust frontend's:\n--- betfe ---\n{}\n--- rustfe ---\n{}",
            String::from_utf8_lossy(&emitted.stdout),
            String::from_utf8_lossy(&dump.stdout),
        );
    }

    println!(
        "selfhost: OK — betfe -> .mir -> backend -> binary prints {expected:?}; \
         .mir byte-identical to the Rust frontend"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke() {
        assert_eq!(2 + 2, 4);
    }

    #[test]
    fn corpus_manifest_is_consistent() {
        // The real tests/corpus tree + MANIFEST.toml must pass the structural lint.
        // This is the same check `cargo xtask corpus --check` runs, so the corpus can
        // never drift out of sync with its manifest without a red test.
        let dir = workspace_root().join("tests").join("corpus");
        let src = std::fs::read_to_string(dir.join("MANIFEST.toml")).unwrap();
        let manifest: CorpusManifest = toml::from_str(&src).unwrap();
        let mut files = Vec::new();
        walk_files(&dir, &mut files).unwrap();
        let bet = stems_with_ext(&files, &dir, "bet");
        let expected = stems_with_ext(&files, &dir, "expected");
        let problems = check_corpus(&manifest, &bet, &expected);
        assert!(
            problems.is_empty(),
            "corpus lint problems:\n{}",
            problems.join("\n")
        );
    }

    #[test]
    fn corpus_detects_orphans_and_gaps() {
        use std::collections::BTreeSet;
        let manifest = CorpusManifest {
            features: CorpusFeatures {
                all: vec!["a".into(), "b".into()],
            },
            program: vec![
                CorpusProgram {
                    path: "cat/one".into(),
                    covers: vec!["a".into()],
                    interp: InterpMode::Pass,
                    compiled: CompiledMode::Skip,
                },
                CorpusProgram {
                    path: "cat/two".into(),
                    covers: vec!["zzz".into()], // unknown feature
                    interp: InterpMode::Pass,
                    compiled: CompiledMode::Skip,
                },
            ],
        };
        // one: paired. two: .bet only (no .expected). three: paired but unlisted.
        let bet: BTreeSet<String> = ["cat/one", "cat/two", "cat/three"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let expected: BTreeSet<String> = ["cat/one", "cat/three"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let problems = check_corpus(&manifest, &bet, &expected);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("cat/two.bet has no matching"))
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("cat/three exists on disk"))
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("feature `b` is not covered"))
        );
        assert!(problems.iter().any(|p| p.contains("unknown feature `zzz`")));
    }

    #[test]
    fn ts_epoch() {
        // 2026-07-02T00:00:00Z — day 20636 since epoch, * 86400.
        assert_eq!(parse_ts("2026-07-02T00:00:00Z"), Some(1_782_950_400));
        // one hour later
        assert_eq!(parse_ts("2026-07-02T01:00:00Z"), Some(1_782_954_000));
        // unix epoch
        assert_eq!(parse_ts("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn idle_clamp() {
        // A single in->out span longer than the cap contributes at most the cap
        // when there are no heartbeats to densify it.
        let spans = vec![SpanFile {
            task: "t".into(),
            session: String::new(),
            events: vec![
                SpanEvent {
                    secs: 0,
                    event: "in".into(),
                    activity: "writing".into(),
                },
                SpanEvent {
                    secs: 10_000,
                    event: "out".into(),
                    activity: "writing".into(),
                },
            ],
        }];
        let totals = compute_totals(&spans, &[], 300);
        assert_eq!(totals.grand, 300);
        assert_eq!(totals.per_activity.get("writing"), Some(&300));
    }

    #[test]
    fn heartbeats_densify() {
        // Same 10k-second span, but heartbeats every 60s keep it "active" so the
        // clamp barely bites — total is close to the real span length.
        let beats: Vec<(i64, String)> = (0..=10_000)
            .step_by(60)
            .map(|s| (s, String::new()))
            .collect();
        let spans = vec![SpanFile {
            task: "t".into(),
            session: String::new(),
            events: vec![
                SpanEvent {
                    secs: 0,
                    event: "in".into(),
                    activity: "writing".into(),
                },
                SpanEvent {
                    secs: 10_000,
                    event: "out".into(),
                    activity: "writing".into(),
                },
            ],
        }];
        let totals = compute_totals(&spans, &beats, 300);
        assert!(totals.grand >= 9_900, "got {}", totals.grand);
    }
}
