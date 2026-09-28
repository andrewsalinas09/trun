# 01 · Vision

## The problem

Long-running work that an AI agent starts is nearly invisible to everyone involved:

- **The human can't see it.** Output is buried in a background shell, a terminal
  that scrolled away, or a remote machine.
- **The agent can't see it either.** It launches `python train.py`, gets distracted
  or its context is compacted, and later reports "training started successfully"
  about a process that died in the first ten seconds.
- **Failures are silent.** Common cases:
  - The process crashed on startup: bad import, CUDA mismatch, missing file.
  - It is alive but hung: deadlock, waiting on stdin, network stall.
  - It is alive but not learning: loss is NaN, loss is flat, the GPU sits at 0% because
    the dataloader is the bottleneck.
  - Output is buffered, so nothing appears for minutes (Python without `-u`).
  - The remote box was preempted or ran out of disk.
- **Nothing is left afterwards.** Once the run is over, there's no record to compare
  against the next attempt.

## The idea

A general-purpose run monitor. Any command, on any machine, gets:

- supervised execution: output capture, exit code, resource and GPU sampling
- structured progress: a step tree, sub-progress, and metrics. This is optional and
  can come from plain output lines or built-in parsers.
- continuous health checks: stall detection, NaN detection, idle-GPU detection,
  failure diagnosis
- live views: a desktop app, a browser, or a phone, all showing the same data
- an AI-native control surface (MCP) so the agent can start, inspect, wait on, and
  react to runs, and read notes from the human
- persistent history for comparing runs

## Goals

- **Zero-config useful.** `trun run -- <cmd>` alone gives logs, status, resources,
  and default stall checks.
- **Progressive structure.** More instrumentation means richer views, but none is
  required.
- **Edge-resilient.** Checks and capture keep working on the remote machine when the
  connection drops. Nothing is lost.
- **AI-first ergonomics.** Every configurable thing is a file the agent can write.
  Every state is queryable as a compact, token-efficient digest. The agent is never
  forced to poll.
- **Single static binary.** One `curl | sh` onto a fresh cloud box with no runtime
  dependencies.
- **Cross-platform.** Windows, Linux (including ARM such as the Pi), and macOS.

## Non-goals (for now)

- Replacing W&B/MLflow experiment tracking: hyperparameter sweeps, model registries,
  and so on. trun keeps scalar metrics and simple artifacts only.
- A full log-analytics platform (Grafana/Loki scale).
- Orchestrating or scheduling across a cluster. trun runs and watches commands, and
  doesn't allocate resources.
- Multiple users, teams, or hosted SaaS. trun is **self-hosted, for one person and
  their own machines**, connected over SSH or a private network such as Tailscale.

## Design stance

trun is designed from first principles for this problem. Nothing is borrowed from an
existing tool and bent to fit. Every piece should be the best answer for trun's needs,
chosen because it is good, not because it saves time.

## Use cases

| Scenario | What trun gives you |
|---|---|
| ML training on a rented GPU | Live loss curves on your desktop, idle-GPU and NaN alerts, preemption detection, and an optional auto-shutdown to stop paying for a dead box |
| Long test suites | Per-test progress, failures surfaced as they happen, and a comparison with the last run |
| Builds and firmware flashing | Step tree (configure → compile → link → flash → verify) with a clear failure point |
| Scrapers and data pipelines | Throughput rate, stall-on-rate-drop, error counts |
| Pi or embedded soak tests | Remote runs visible from the desktop, with temperature and resource tracking |
| Agent-driven work in general | The agent starts a task, calls `trun wait`, and is woken up on completion, failure, or stall instead of polling or forgetting |

## What success looks like

- The agent never again claims a run succeeded when it didn't.
- A stalled or wasted GPU run is noticed within minutes, not hours.
- Glancing at one window answers "what is running everywhere, and is it healthy?"
