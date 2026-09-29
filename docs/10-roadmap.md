# 10 · Roadmap

Each milestone must be usable on its own. The first three alone solve the core
problem: invisible, silently failing runs.

## M1 · Local supervisor + hub + minimal UI

- `trun run` (pipes, process group/Job Object, unbuffer env, detach, cancel)
- `trun hub` with an embedded agent, SQLite store, and local IPC
- Web UI: run list, run page with live log tail, lifecycle, exit code, duration
- `trun ls`, `status`, `logs -f`, `cancel`
- Early-death detection plus the diagnosis parser

**Done when:** a Python script that crashes after 3 s, one that hangs, and one that
succeeds are all visible in the browser with correct states. Closing the launching
terminal doesn't kill a detached run. Windows and Linux both work.

**Status: done (2026-09-28).** Verified on Windows 11 and on Linux (a static musl
build in WSL): success, import-error crash, missing program, SIGSEGV, SIGKILL, a
hang plus cancel of the whole process tree, and a detached run and hub surviving the
launching shell. Known gaps carried forward:
- Windows cancel is a hard kill of the Job Object. Graceful Ctrl-Break needs a
  shared console, so it is deferred.
- Runs are owned by the hub process. If the hub dies, active runs are marked `lost`
  on restart (reconnection to still-running processes is planned with M4's agent).
- `--pty` moved to M2 as planned. stdin is always null.

## M2 · Structure: protocol, parsers, panels

- `::` protocol (steps, progress, metrics, notes, heartbeat, expect), `trun emit`
- Side channel (`TRUN_EVENTS`) plus the Python helper
- Built-in parsers: tqdm, generic-fraction, pytest, cargo
- Step tree UI, auto metric panels, TOML panels, dashboards, hot reload
- `--pty` mode

**Done when:** an unmodified tqdm training loop shows progress and ETA, and a TOML
panel edited in an editor re-renders within 1 s.

**Status: done (2026-09-29).** Verified on Windows 11 and Linux (WSL, static musl):
- An unmodified tqdm loop gives a step per epoch, progress, rate, ETA, and `loss`
  from the postfix, both with pipes and under `--pty`.
- Real pytest (14 tests, failures, and metrics) and `cargo test` (a step per test
  binary).
- The `::` protocol, including NaN metrics and a malformed line surfaced as a
  warning.
- The Python helper over the side channel (named pipe on Windows, 0700 Unix socket
  on Linux), and `trun emit` from bash.
- Panel hot reload measured at about 100 ms. A broken TOML edit surfaces as a
  per-file error with line and column.

Beyond the plan:
- A Windows ConPTY driver with passthrough mode where available and a normalizer
  otherwise (D22).
- The hub runs from a copy of the executable, with automatic restart when trun is
  updated (D23).
- A handle-inheritance fix so output capture of `trun` can't hang (D24).
- A per-run `messages` feed (notes, warnings, and errors).
- Collapsing long step lists in the UI.

Deferred:
- jest/vitest, HF Trainer, Lightning, and Keras parsers.
- Panel sources other than `metric`.
- TS panels (M6).

## M3 · Checks, stall detection, wait

- Starlark check engine, time-series API, alert hysteresis, health axis
- Default checks (silence, no-progress, zombie, NaN, disk, memory)
- `trun wait` with exit codes and digest output
- `trun check test` replay
- Desktop notifications

**Done when:** a script that goes silent is marked stalled within the configured
window and `trun wait --until stalled` returns with exit code 2. A NaN metric fails
the run's health. Replay reproduces both from recorded data.

**Status: done (2026-09-29).** Verified on Windows 11 and Linux (WSL, static musl):
- A silent script is stalled 10 s in, with the limit at 8 s, and `wait` exits 2.
- A NaN metric makes health `failing`, and `wait --until failing` exits 6.
- `check test builtin:defaults` replays both at the right offsets.
- The zombie alert fires from real process-tree CPU samples.
- A check with `kill: True` stops the run, with diagnosis `killed-by-check`.
- A broken check surfaces as `check_error` without affecting the run.
- Desktop notifications work on Windows. Without a D-Bus session they are skipped
  with one warning.

Beyond the plan:
- `trun check lint`.
- Thresholds in `config.toml`.
- A `builtin:alerts` dashboard cell.
- Alerts in the UI's messages feed.
- A `failing` wait condition (exit 6).

Deferred:
- Recording samples so replays see CPU, memory, disk, and GPU (M4).
- GPU checks (M4, needs NVML).
- `shutdown_host` (M7).
- `load()` of shared Starlark helpers.
- Hub-side agent-silence checks (M4).

## M4 · Remote agents

- `trun agent` over SSH (`trun hosts add`, `--install`, `--stdio`) and join mode (outbound WSS), heartbeats
- Agent-side configurable downsampling (t-digest percentiles, moments, NaN preservation)
- User-level daemons (systemd --user with linger, Scheduled Task, LaunchAgent), `trun doctor`
- Spool and resume with gap backfill
- NVML GPU sampling, per-process GPU attribution, GPU default checks
- Remote exec (opt-in), config sync of `.trun/` to the hub
- Static musl builds (x86_64, aarch64), `install.sh`, `install-service`
- Fleet view

**Done when:** a run on the Pi and a run on a cloud GPU box show live on the desktop.
Pulling the network for 5 minutes loses no metrics. The Pi run is started from the
desktop CLI.

## M5 · MCP

- `trun mcp` with run, notes, structure, errors, and fleet tools
- Digest formatting shared by status, wait, and MCP
- `write_*` tools with lint and first-evaluation feedback
- Claude Code integration guide (the wake-up pattern)

**Done when:** Claude starts a training run on gpu1, waits in the background, is woken
by a stall, diagnoses it from `tail_logs` and `query_metric`, and leaves a note, all
without manual prompting.

## M6 · Desktop app & polish

- Tauri shell with a tray icon (fleet health color), native notifications, deep links
- TS panels (hub-side oxc compile, sandboxed iframe, optional tsgo type check)
- Run comparison view, `trun diff`
- Phone-friendly UI plus ntfy push
- Task templates with args

## M7 · Cloud extras

- Preemption detection (AWS/GCP/Azure plus the SIGTERM fallback)
- Cost tracking and `idle_shutdown_after`, `shutdown_host` action
- Artifact upload plus gallery panels
- AMD ROCm sampling

## Later / maybe

- Helper libraries for Rust and TypeScript
- Ingesting TensorBoard event files as metrics
- Run-to-run automatic regression detection ("this test got 40% slower")
- Plugin marketplace for parsers and panels
