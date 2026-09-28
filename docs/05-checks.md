# 05 · Checks & stall detection

Checks are small [Starlark](https://github.com/facebook/starlark-rust) scripts,
a restricted dialect of Python, evaluated continuously **on the agent that owns the
run**. They keep working when the hub is unreachable, the laptop is asleep, or no one
is watching. They turn raw state into **health** and **alerts**, and can take actions.

The heavy lifting (series math, regex, percentiles) is native Rust exposed as host
functions. Starlark is only the thin rule logic, so it is fast (see D19).

## Anatomy of a check

```python
# .trun/checks/training.star
META = {
    "applies": "train*",   # glob on run name or template; "*" = all runs
    "every": secs(30),     # evaluation interval (default 10s)
    "grace": mins(2),      # don't evaluate until the run is this old
}

def check(run):
    loss = run.metric("loss")
    if is_nan(loss.last()):
        fail("loss went NaN")
    if loss.slope(mins(15)) > -0.0001 and run.elapsed() > mins(30):
        warn("loss has plateaued for 15 min")
    if run.host.gpu.util.avg(mins(5)) < 5.0:
        stalled("GPU idle for 5 min (dataloader bottleneck or hang?)")
```

One file can hold many rules. Each call to an action function produces an alert
keyed by `(check file, message template)`.

## The time-series API

Everything numeric is a **series**, a sequence of timestamped values.

| Source | Expression |
|---|---|
| Run metrics | `run.metric("loss")` |
| Step progress | `run.progress("train")` (series of `current`), `run.progress("train").rate(mins(5))` |
| Output activity | `run.output.silence()` (duration since last output line or heartbeat), `run.output.lines_per_min()` |
| Log matches | `run.logs.count(`(?i)error`, mins(10))` |
| Run process | `run.proc.cpu`, `run.proc.mem`, `run.proc.threads`, `run.proc.io_read`, `run.proc.io_write` |
| Host | `run.host.cpu`, `run.host.mem`, `run.host.disk_free("/data")`, `run.host.gpu.util`, `.gpu.mem`, `.gpu.temp`, `.gpu.power` (all GPUs used by the run; `run.host.gpu[i]` for one) |
| Run facts | `run.elapsed()`, `run.name`, `run.tags`, `run.lifecycle`, `run.step_current()` |

Series methods: `last()`, `avg(d)`, `min(d)`, `max(d)`, `slope(d)` (per second, by
least squares), `delta(d)`, `rate(d)`, `count(d)`, `stddev(d)`, `is_nan()`,
`age()` (time since last point), `exists()`.

Durations: `secs(n)`, `mins(n)`, `hours(n)`. They compare against each other and
against `run.elapsed()`.

## Actions

| Action | Effect |
|---|---|
| `info(msg)` | Alert at info level. Shown in the UI, not in health |
| `warn(msg)` | Health becomes at least `warn` |
| `stalled(msg)` | Health becomes `stalled`. Wakes `trun wait --until stalled` |
| `fail(msg)` | Health becomes `failing`. With `kill: true` in meta, it also cancels the run |
| `kill(msg)` | Cancels the run immediately and records the reason |
| `notify(msg)` | Desktop/phone notification regardless of level |
| `shutdown_host(msg)` | Powers off the host. **Only when the agent config sets `allow_shutdown = true`.** Meant for costly cloud boxes |

## Alert lifecycle and hysteresis

Checks are evaluated repeatedly, so alerts are **edge-triggered with hysteresis**:

- An alert **opens** the first time its action is called. That produces one event and
  one notification.
- While it keeps firing, it stays open. Only `last_at` updates, so there is no spam.
- It **clears** after it has not fired for `clear_after` consecutive evaluations
  (default 3). Clearing produces an event.
- Health is derived from the worst open alert.

## Default checks

These are shipped in the global config (`~/.trun/checks/defaults.star`) and apply to
every run unless `--no-default-checks` is used or they are overridden.

| Check | Rule | Action |
|---|---|---|
| Silence | No output or heartbeat for 5 min (respects `::expect silence=`) | `stalled` |
| No progress | A progress-tracked step hasn't advanced in 15 min | `stalled` |
| Zombie | Process alive, CPU < 1%, no GPU activity, no output, for 10 min | `stalled` |
| Early death | Exited non-zero within 60 s of start | diagnosis highlighted as "failed to start" |
| NaN / Inf | Any metric's last value is NaN or Inf | `fail` |
| Disk | Free space on the cwd volume < 2 GB | `warn`; < 200 MB → `fail` |
| Memory | Host memory > 95% for 2 min | `warn` (OOM likely) |
| GPU idle | Run uses a GPU and utilization < 5% for 10 min | `stalled` |
| GPU thermal | GPU temp > 88 °C for 5 min | `warn` |

Default thresholds can be changed in `~/.trun/config.toml` under `[defaults.checks]`
without editing the script.

## Hub-side checks

Some conditions can only be seen from the hub:

- **Agent silence:** no events or heartbeats from an agent for 90 s marks its runs
  `health=stalled` ("connection lost"). After `lost_after` (default 30 min) without
  reconnecting, the lifecycle becomes `lost`.
- **Wall-clock budget:** optional `max_duration` on a task template.

## Failure diagnosis

When a run ends non-zero, the diagnosis pass looks at the exit code or signal, the
last 200 lines of output, and open alerts, and produces a short classified cause:

`oom-killed` · `cuda-oom` · `import-error` · `file-not-found` · `permission-denied` ·
`segfault` · `disk-full` · `killed-by-check` · `preempted` · `timeout` · `test-failures` ·
`unknown`

Each diagnosis includes the **evidence**: the matching lines, with sequence numbers.
It is the first thing a digest shows for a failed run.

## Developing checks

```sh
trun check test .trun/checks/training.star --run train-v3   # replay against recorded data
trun lint .trun/checks/
```

A replay evaluates the check on each tick of the recorded run and prints when each
alert would have opened and cleared. That makes it quick to tune thresholds.
Runtime errors in a check become `check_error` events on the run (visible in the UI
and to the agent over MCP). They never crash the agent.

## Sandbox

Starlark is hermetic by design. The language has no filesystem, network, process,
clock, or randomness primitives. The only side effects are the host functions trun
exposes (the actions above). In addition:

- **Termination:** Starlark has no `while` loops or recursion by default, so most
  scripts terminate by construction. A cancellation budget
  (`Evaluator::set_check_cancelled`, time-based) guards against pathological
  `for` loops over huge ranges.
- **Memory:** each evaluation runs on its own heap, with a size limit.
- **Errors** come with file, line, column, and a caret-underlined snippet, in the
  style of rustc diagnostics. They are passed verbatim to the UI and to
  `get_errors`, so an AI can fix its own check.

## Language notes for authors

- The syntax is Python, but it is a restricted dialect: no `while`, no recursion,
  no classes, no `import` (use `load()` for shared helpers in `.trun/lib/*.star`),
  no exceptions, and top-level values are frozen after load.
- Constants such as `META` and compiled regexes are defined at the top level. They
  are evaluated once when the file loads.
- Host helpers: `secs()`, `mins()`, `hours()`, `regex(pattern)` (returns an object
  with `.match()`, `.search()`, `.findall()`), `is_nan()`, `is_inf()`, and the action
  functions.
