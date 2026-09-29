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
    trend = loss.slope(mins(15))            # None until there are 2+ points
    if trend != None and trend > -0.0001 and run.elapsed() > mins(30):
        warn("loss has plateaued for 15 min")
    gpu = run.gpu_util().avg(mins(5))       # None when there is no GPU data
    if gpu != None and gpu < 5.0:
        stalled("GPU idle for 5 min (dataloader bottleneck or hang?)")
```

Try it against a finished run without waiting for a live failure:
`trun check test .trun/checks/training.star --run train-v3`.

One file can hold many rules. Each action call produces an alert identified by the
check name plus the message with its numbers normalized ("no output for 5m03s" and
"no output for 5m13s" are the same alert). An explicit `key="…"` argument overrides
this.

## The check API (implemented in M3)

**Units: durations are seconds everywhere.** `mins(15)` is `900`. `run.elapsed()`,
`run.silence()` and `series.age()` return seconds. `slope()` and `rate()` are per
second. Aggregations over a window with no data return `None` instead of raising, so
test with `!= None` (or `is_nan()`, which is `False` for `None`).

### The run

| Expression | Returns |
|---|---|
| `run.id`, `run.name`, `run.project` | strings |
| `run.lifecycle` | `queued`, `starting`, `running`, `succeeded`, `failed`, `cancelled`, `lost`, `preempted` |
| `run.elapsed()` | seconds since the run started |
| `run.silence()` | seconds since the last output line, heartbeat, or structured event (`None` before start) |
| `run.expect_silence()` | seconds set by `::expect silence=…`, or `None` |
| `run.lines_per_min(window=60)` | output rate |
| `run.metric(name)` | a **series**. It is empty if the run never reported that metric |
| `run.metrics()` | sorted metric names |
| `run.progress(id=None)` | series of a step's `current`, recorded when it changes. Without an id: the latest running step with progress |
| `run.steps()` | list of dicts: `id`, `name`, `parent`, `running`, `current`, `total`, `unit`, `age` |
| `run.proc_cpu()`, `run.proc_mem()` | CPU % (100 = one core) and resident bytes of the run's **whole process tree** |
| `run.host_cpu()`, `run.host_mem()` | host CPU %, and memory used % |
| `run.disk_free()` | free bytes on the volume holding the working directory |
| `run.gpu_util()`, `run.gpu_mem()` | GPU series. Empty until GPU sampling lands (M4) |
| `run.config(key, default)` | a threshold from `[defaults.checks]` in config.toml (durations as seconds) |

Resources are sampled every 5 s by the agent.

### Series methods

`last()` · `avg(window=None)` · `min(window)` · `max(window)` · `stddev(window)` ·
`slope(window)` (least squares, per second) · `delta(window)` · `rate(window)` ·
`count(window)` · `count_non_finite(window)` · `age()` (seconds since the latest
point) · `exists()`

`window` is in seconds. Without one, the whole retained history is used (series keep
2 h, capped at 200k points). `last()` can be NaN: detecting that is the point.

### Globals

`secs(n)`, `mins(n)`, `hours(n)` · `is_nan(x)`, `is_inf(x)`, `is_finite(x)` (all
`False` for `None`) · `fmt_duration(seconds)` → `"4m12s"`

### META

```python
META = {
    "applies": "train*",   # glob on the run name (default: every run)
    "every": secs(30),     # evaluation interval, default 10 s, minimum 1 s
    "grace": mins(2),      # skip until the run is this old
    "kill": True,          # fail() also stops the run
    "clear_after": 3,      # quiet evaluations before an alert clears
}
```

Unknown META keys are errors, so a typo like `evry` is caught.

## Actions

| Action | Effect |
|---|---|
| `info(msg)` | Alert at info level. Shown in the UI, not in health |
| `warn(msg)` | Health becomes at least `warn` |
| `stalled(msg)` | Health becomes `stalled`. Wakes `trun wait --until stalled` |
| `fail(msg)` | Health becomes `failing`. With `kill: true` in meta, it also cancels the run |
| `kill(msg)` | Stops the run (graceful, then forced). It ends `failed` with diagnosis `killed-by-check` |
| `notify(msg)` | Desktop notification regardless of level (no alert) |
| `shutdown_host(msg)` | *(planned, M7)* Powers off the host. **Only when the agent config sets `allow_shutdown = true`.** Meant for costly cloud boxes |

## Alert lifecycle and hysteresis

Checks are evaluated repeatedly, so alerts are **edge-triggered with hysteresis**:

- An alert **opens** the first time its action is called. That produces one event and
  one notification.
- While it keeps firing, it stays open. Only `last_at` updates, so there is no spam.
- It **clears** after it has not fired for `clear_after` consecutive evaluations
  (default 3). Clearing produces an event.
- Health is derived from the worst open alert.

## Default checks

They are built into the binary
([`crates/trun-checks/src/defaults.star`](../crates/trun-checks/src/defaults.star),
Starlark using the same API) and apply to every run unless `--no-default-checks` is
used. A `checks/defaults.star` in the project's `.trun/` or in `$TRUN_HOME` replaces
them. `trun check lint` lists what is active.

| Check | Rule | Action |
|---|---|---|
| Silence | No output or heartbeat for 5 min (respects `::expect silence=`) | `stalled` |
| No progress | A progress-tracked step hasn't advanced in 15 min | `stalled` |
| Zombie | Process tree alive, CPU < 1% (sampled), no GPU activity, no output, for 10 min | `stalled` |
| Early death | Exited non-zero within 60 s of start | diagnosis highlighted as "failed to start" |
| NaN / Inf | Any metric had a NaN/Inf value in the last 5 min | `fail` (clears 5 min after the last bad value) |
| Disk | Free space on the cwd volume < 2 GB | `warn`; < 200 MB → `fail` |
| Memory | Host memory > 95% for 2 min | `warn` (OOM likely) |
| GPU idle | Run uses a GPU and utilization < 5% for 10 min | `stalled` (active once GPU sampling exists, M4) |
| GPU thermal | GPU temp > 88 °C for 5 min | `warn` (planned, M4) |

Thresholds can be changed in `$TRUN_HOME/config.toml` without editing the script
(durations as `"90s"`/`"5m"`, or numbers):

```toml
[defaults.checks]
silence = "5m"          # no output/heartbeat/events
no_progress = "15m"     # a progress-reporting step stopped moving
zombie = "10m"          # idle, silent process tree
nan_window = "5m"       # how long a NaN keeps the run failing
disk_warn_bytes = 2e9
disk_fail_bytes = 200e6
memory_pct = 95
memory_window = "2m"
gpu_idle = "10m"
```

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
trun check lint                                   # this project's + global checks + builtins
trun check lint .trun/checks/training.star        # specific files
trun check test .trun/checks/training.star --run train-v3 [--with-defaults]
trun check test builtin:defaults --run train-v3   # how the defaults saw that run
```

A replay feeds the recorded events into the same `RunData` the live agent uses, and
evaluates the check on its own schedule from start to end. It prints when each alert
would have opened, been updated, or cleared:

```
replaying defaults against run 01M3P… "m3-hang" (22.7s, 5 events, every 5s)
    +10.0s  opened   [stalled] no output for 10s (limit 8s)
summary: 5 evaluations · 1 alert(s) opened · 1 open at the end · health stalled
```

Process, host, and GPU samples are not recorded yet, so those series are empty in
replays.

**Where checks come from**, by file stem with the first match winning:
1. `--check FILE` on `trun run`
2. the project's `.trun/checks/*.star`
3. `$TRUN_HOME/checks/*.star`
4. the built-in defaults

Files are re-scanned every 3 s. A broken edit reports a `check_error` and keeps the
previous version running. Deleting a file clears its alerts.

Runtime errors in a check become `check_error` events on the run, shown once per
distinct error in the UI's messages and available over MCP. They never crash the
agent or affect the run.

**Notifications**: when an alert opens or escalates to `stalled` or `fail`, on
`notify()`, when a run fails, and when a run succeeds after 5+ minutes, the hub
sends a native desktop notification (Windows toast, freedesktop on Linux, macOS
Notification Center). Identical notifications within 30 s are dropped. Turn them off
with `[notify] desktop = false`.

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
