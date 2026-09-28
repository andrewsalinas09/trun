# 02 · Architecture

## Components

There is one binary, `trun`, with several roles:

| Role | Command | Responsibility |
|---|---|---|
| **Supervisor** | `trun run` | Spawns the command, captures stdout/stderr, parses the progress protocol, and forwards events to the local daemon |
| **Agent** | `trun agent` | Per-machine daemon. Owns the runs on its host, samples system/GPU stats, evaluates checks, spools events, and streams them to the hub |
| **Hub** | `trun hub` | Aggregator. Includes an agent for its own host, stores history, serves the API and web UI, and relays commands to agents |
| **MCP server** | `trun mcp` | A stdio MCP server that is a thin client of the local hub API |
| **Desktop app** | Tauri shell | A native window around the same web UI the hub serves |

A typical setup is a hub on the desktop plus agents on remote hosts. A lone laptop
just runs `trun hub`, which includes a local agent.

## Topology

```
 Remote host (GPU cloud, Pi, CI)             Desktop / hub host
 ┌─────────────────────────────┐             ┌──────────────────────────────┐
 │ trun agent                  │ SSH channel │ trun hub                     │
 │  ├─ supervisor(s)           │  or WSS     │  ├─ embedded agent (local)   │
 │  ├─ system + GPU sampler    │ ──────────▶ │  ├─ SQLite history           │
 │  ├─ checks (Rhai)           │ ◀────────── │  ├─ HTTP + WS API            │
 │  ├─ parsers                 │  commands   │  ├─ web UI (static assets)   │
 │  └─ SQLite spool            │             │  └─ file watcher (.trun/)    │
 └─────────────────────────────┘             └───────▲──────────▲───────────┘
                                                     │          │
                                         Tauri / browser    trun mcp ◀── AI agent
                                         / phone            trun wait / CLI
```

- **Single user, self-hosted.** Remote hosts connect over the user's SSH (the hub
  dials in) or a tailnet (the agent dials out). Nothing is exposed publicly. See
  [07 Remote](07-remote.md).
- **Local IPC.** `trun run` and other CLI commands talk to the local daemon over a
  Unix domain socket (Linux/macOS) or a named pipe (Windows).
- **If no daemon is running,** `trun run` starts an ephemeral agent in the
  background, so the command always works.

## Data flow for one run

1. `trun run -- cmd` asks the local daemon to create a run, which gets a ULID.
2. The supervisor spawns `cmd` in a new process group (a Job Object on Windows). It
   sets `PYTHONUNBUFFERED=1` and related variables by default, and `TRUN_RUN_ID`
   plus `TRUN_EVENTS` for the structured side channel.
3. Every output line gets a sequence number and timestamp, goes to the parsers, and
   comes out as a stream of **events**.
4. The agent adds **samples** (CPU, memory, disk, GPU) every N seconds and evaluates
   **checks** on each tick.
5. Events are written to the local spool, then streamed to the hub and acknowledged
   by sequence number.
6. The hub stores them and pushes them live to subscribed UIs and waiters.
7. When the process exits, the supervisor records the exit code and signal. A
   diagnosis pass classifies the failure cause (OOM kill, CUDA OOM, import error,
   segfault, and so on).

## Data model

```
Host      id, name, os, arch, gpus[], agent_version, last_seen, status(online|offline)
Run       id(ULID), name, host_id, template?, cmd, cwd, env_digest,
          lifecycle, health, started_at, ended_at, exit_code, signal,
          diagnosis?, parent_run?        -- for runs spawned by runs
Step      run_id, id, parent_id?, name, state, current?, total?, unit?, started_at, ended_at
Event     run_id, seq, ts, kind, payload  -- append-only log of everything
Metric    run_id, name, ts, step?, value  -- denormalized from events for fast queries
Sample    host_id, ts, cpu, mem, disk, net, gpu[{util, mem, temp, power}]
Alert     run_id, check, level, message, first_at, last_at, cleared_at?
Note      run_id, author(human|agent), ts, text, read_by_agent_at?
Artifact  run_id, path, kind, size, hash, stored_at
```

### Run state is two separate axes

- **Lifecycle:** `queued → starting → running → {succeeded | failed | cancelled | lost | preempted}`
- **Health:** `ok | warn | stalled | failing`. Checks set this, and it only has meaning
  while the run is `running`.

Keeping them apart avoids the ambiguity of states like "running but stalled". The UI
and digests show both.

`lost` means the agent stopped reporting and never came back within the timeout.
`preempted` means the cloud provider signaled termination. Both are distinct from
`failed`.

### Event kinds

`output` (stream, text) · `step_begin` · `step_end` · `progress` · `metric` ·
`log` (level, text) · `artifact` · `note` · `lifecycle` · `health` · `alert` ·
`diagnosis` · `check_error` · `panel_error`

## Tech stack

| Area | Choice | Rationale |
|---|---|---|
| Language (core) | Rust | Single static binary (musl on Linux), low overhead, safe for a long-lived daemon |
| Async runtime | tokio | Standard |
| HTTP / WS | axum + tokio-tungstenite | Standard, and good for SSE/WS |
| Wire encoding | MessagePack (rmp-serde) | Compact, schema shared through serde types |
| Storage | SQLite (rusqlite, WAL mode) | Hub history and agent spool alike, with zero ops |
| Check/parser scripting | Rhai | Embeddable, sandboxed, JS-like syntax that AI writes well, runs headless on the agent |
| System stats | sysinfo | Cross-platform |
| GPU stats | nvml-wrapper (NVIDIA); ROCm SMI later | |
| File watching | notify | Hot reload of `.trun/` |
| TS panel compiler | oxc | Native Rust TS/TSX transform and bundling on the hub. Nothing heavy in the browser |
| Remote transport | russh (SSH mode), tokio-tungstenite (join mode) | SSH reuses the user's existing keys and config |
| Downsampling | t-digest sketches + streaming moments | User-chosen aggregates, including any percentile, at bounded memory |
| MCP | rmcp (official Rust SDK) | |
| CLI | clap | |
| UI framework | Svelte 5 + Vite + TypeScript | Small, fast, easy for AI to edit |
| Charts | Observable Plot | Concise declarative API, both for compiling TOML panels and for hand-written TS panels |
| Desktop shell | Tauri 2 | Native window and tray icon, notifications, small binary |

## Planned repository layout

```
trun/
  crates/
    trun-proto/      serde types for events, commands, wire protocol
    trun-store/      SQLite schema + queries (hub + spool)
    trun-supervise/  process spawning, pty, output capture, job objects
    trun-parse/      :: protocol + built-in parsers + Rhai parser host
    trun-checks/     Rhai check engine, time-series API, alert state machine
    trun-agent/      agent daemon, samplers, spool, uplink
    trun-hub/        hub daemon, API, fan-out, command relay
    trun-mcp/        MCP server
    trun/            the binary (clap CLI wiring all roles)
  ui/                Svelte web UI (embedded in hub binary via include_dir/rust-embed)
  app/               Tauri shell
  docs/
```

## Process capture details

- **Pipes by default, pty with `--pty`.** Some tools, such as tqdm and colored
  output, behave differently without a TTY. With `--pty` the command runs under a
  pseudo-terminal (ConPTY on Windows), and ANSI codes are stripped for parsing but
  kept for display.
- **Carriage-return handling.** Progress bars that redraw with `\r` are collapsed
  into a single updating line rather than thousands of log lines.
- **Unbuffering.** Defaults: `PYTHONUNBUFFERED=1` and `NODE_NO_WARNINGS` untouched,
  with an opt-in `stdbuf -oL` wrapper on Linux. Buffering is one of the most common
  "it's not doing anything" causes.
- **Kill semantics.** Cancel sends a graceful signal to the whole process tree
  (SIGTERM, or CTRL_BREAK on Windows), then a hard kill after a grace period
  (default 10 s).
- **Detach.** Runs belong to the agent, not to the terminal that started them.
  Closing the terminal or ending the AI session does not kill the run.
