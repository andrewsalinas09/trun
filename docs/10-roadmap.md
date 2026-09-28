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

## M2 · Structure: protocol, parsers, panels

- `::` protocol (steps, progress, metrics, notes, heartbeat, expect), `trun emit`
- Side channel (`TRUN_EVENTS`) plus the Python helper
- Built-in parsers: tqdm, generic-fraction, pytest, cargo
- Step tree UI, auto metric panels, TOML panels, dashboards, hot reload
- `--pty` mode

**Done when:** an unmodified tqdm training loop shows progress and ETA, and a TOML
panel edited in an editor re-renders within 1 s.

## M3 · Checks, stall detection, wait

- Starlark check engine, time-series API, alert hysteresis, health axis
- Default checks (silence, no-progress, zombie, NaN, disk, memory)
- `trun wait` with exit codes and digest output
- `trun check test` replay
- Desktop notifications

**Done when:** a script that goes silent is marked stalled within the configured
window and `trun wait --until stalled` returns with exit code 2. A NaN metric fails
the run's health. Replay reproduces both from recorded data.

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
