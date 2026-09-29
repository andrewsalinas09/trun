# trun

**Live visibility into long-running tasks, shared by humans and AI agents.**

When an AI agent kicks off a test suite, a build, or a training run, the work often
disappears into a black box. The job crashes, never really starts, or quietly hangs,
and nobody notices. That includes the agent, which may report success anyway.

trun wraps any command in a supervisor that records everything about it: output,
exit status, resource usage, progress, and metrics. It watches the run for stalls
and failures, and shows it all live on your desktop, in a browser, on your phone,
and to your AI agent over MCP. Runs can be local or on remote machines such as a GPU
cloud box, a Raspberry Pi, or a CI runner.

```sh
# Local: zero instrumentation needed
trun run --name train-v3 -- python train.py --epochs 50

# Add a remote box you can already ssh into (installs trun there too)
trun hosts add gpu1 --install
trun run --host gpu1 -- python train.py

# Agent (or you) blocks until something happens worth reacting to
trun wait train-v3 --until done,failed,stalled
```

## Core principles

1. **Supervision first, instrumentation second.** The supervisor owns the process,
   so a run that crashes or never produces output is still visible. Structure such
   as steps, progress, and metrics is added on top, but it isn't needed.
2. **One shared source of truth.** The human and the agent look at the same state.
   The agent can check on a run without guessing, and the human can leave notes the
   agent will read.
3. **Stall detection is a first-class feature.** "Still running" is not the same as
   "making progress". Checks run continuously at the edge, on the machine doing the
   work, even when nobody is watching.
4. **Everything configurable is a file.** Panels, checks, parsers, and task templates
   live in `.trun/` as plain files that hot-reload. The agent customizes monitoring
   by editing files, the same way it edits code.
5. **Local and remote are the same thing.** Every machine runs the same binary. Hosts
   connect over your existing SSH or tailnet. Runs survive disconnects, and adding an
   ephemeral cloud box costs nothing.
6. **Self-hosted, single user.** Your machines, your data. There's no service to sign
   up for.

## Documentation

| Doc | Contents |
|---|---|
| [01 Vision](docs/01-vision.md) | Problem, goals, non-goals, use cases |
| [02 Architecture](docs/02-architecture.md) | Components, topology, tech stack, data model |
| [03 CLI](docs/03-cli.md) | Command reference |
| [04 Progress protocol](docs/04-progress-protocol.md) | `::` output lines, side channel, built-in and custom parsers |
| [05 Checks & stall detection](docs/05-checks.md) | Starlark check scripts, the time-series API, actions, defaults |
| [06 Panels & dashboards](docs/06-panels.md) | TOML panels, TypeScript panels, dashboards, hot reload |
| [07 Remote & cloud](docs/07-remote.md) | Agents, transport, spool and resume, auth, preemption, cost control |
| [08 MCP interface](docs/08-mcp.md) | Tools the AI agent uses, digests, the wake-up pattern |
| [09 Project layout](docs/09-project-layout.md) | The `.trun/` directory, task templates, config resolution |
| [10 Roadmap](docs/10-roadmap.md) | Milestones with acceptance criteria |
| [11 Open questions](docs/11-open-questions.md) | Unresolved decisions |
| [Decisions](docs/decisions.md) | Log of decisions made so far, with rationale |

## Status

**M1–M3 are implemented** on Windows and Linux:
- **M1:** the local supervisor, hub, SQLite store, and web UI.
- **M2:** structure: the `::` protocol, built-in parsers, the side channel, `--pty`,
  charts, and hot-reloading panels.
- **M3:** checks: stall/zombie/NaN/disk/memory detection in Starlark, alerts, health,
  desktop notifications, `trun wait`, and check replay.

See the [roadmap](docs/10-roadmap.md). Remote hosts (`trun hosts`) and MCP come in
later milestones. The examples above that use them describe the design.

What works today:

```sh
trun run -- python train.py          # attached: streams output, exits with the run's code
trun run -d --name nightly -- make   # detached: prints the run id, keeps running
trun run --pty -- ./needs-a-tty      # under a pseudo-terminal (ConPTY / openpty)
trun ls [-a]                         # runs, newest first, with live progress
trun status nightly                  # digest: state, diagnosis, step tree, metrics, last output
trun logs -f nightly [--grep re]     # output, then follow live
trun cancel nightly [--force]        # stop the whole process tree
trun emit progress build 3 10        # report structure from shell scripts
trun wait nightly --until done,stalled   # block until something happens (exit code says what)
trun check test my.star --run nightly    # replay a check against a recorded run
trun ui [run]                        # open the web UI
trun hub start|stop|status           # the hub auto-starts on first use
```

Structure comes for free from tqdm, pytest, and cargo output. Anything else can
print `::progress train 3/10`, `::metric loss=0.31 step=3`, `::step-begin eval`
(see [04](docs/04-progress-protocol.md)), or use the dependency-free
[Python helper](sdk/python/trun.py). Panels and dashboards are TOML files in
`.trun/`. Try `trun run --cwd examples/training -- python train.py`, then `trun ui`.

## Development

```sh
cd ui && npm install && npm run build   # the UI is embedded into the binary from ui/dist
cd .. && cargo build                    # without a built UI, a placeholder page is embedded
cargo test
```

- `TRUN_HOME` relocates all state (default `~/.trun`), which is useful for isolated
  test runs.
- `TRUN_LOG=debug` raises hub log verbosity. The auto-started hub logs to
  `~/.trun/hub.log`.
- Static Linux builds: `cargo zigbuild --release -p trun --target x86_64-unknown-linux-musl`
  (or `aarch64-unknown-linux-musl`).
- On Windows, stop the hub (`trun hub stop`) before rebuilding, because a running
  `trun.exe` can't be overwritten.
