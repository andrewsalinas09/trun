# 07 · Remote & cloud

The flagship remote scenario is a training job on a rented GPU box, watched from the
desktop and from an AI agent. The same mechanism covers a Pi, a CI runner, or a
second workstation.

## Scope: one person, their own machines

trun is self-hosted and single-user. There is no relay service, no multi-tenant auth,
and no public exposure. Connectivity reuses what you already have: **SSH** and/or a
private network such as **Tailscale or WireGuard**.

## Principles

1. **Reuse existing trust.** If you can `ssh gpu1`, trun can reach gpu1. No new ports
   are opened to the internet.
2. **The edge is autonomous.** Capture, checks, and cost-protection actions run on the
   agent and don't depend on the connection.
3. **Nothing is lost.** Events are spooled locally and replayed after reconnecting.
4. **Setup is one line.**

## Two ways to connect a host

| Mode | Who dials | Auth | Best for |
|---|---|---|---|
| **SSH** (default) | Hub → host, over your SSH config | Your SSH keys | Any box you can already `ssh` into: cloud GPUs, the Pi, servers |
| **Join** | Agent → hub, WSS over tailnet/LAN | Per-agent credential from a join token | Boxes that can reach your tailnet but that you can't SSH into from the hub (e.g. behind double NAT without Tailscale SSH) |

### SSH mode

```sh
trun hosts add gpu1                   # uses the "gpu1" entry in ~/.ssh/config
trun hosts add gpu1 --install         # also copies the right trun binary to the host
```

The hub opens an SSH connection and starts `trun agent --stdio` on the remote. The
protocol runs over that SSH channel, and the remote agent keeps running as a user
daemon after the channel drops. On reconnect the hub attaches again and the spool
resumes. The hub retries with backoff while the host is unreachable. No tokens and no
listening ports are involved: SSH is the auth.

`--install` detects the remote OS and architecture, uploads the matching static
binary, and installs the user service (see *Daemon model* below).

### Join mode

On the hub:

```sh
trun hub token create --name gpu1 --ttl 1h --allow-exec
# → trun_join_7Hk…   (single-use; expires in 1h)
```

On the remote host (for example in a cloud-init or startup script):

```sh
curl -fsSL https://<release-url>/install.sh | sh
trun agent --join wss://desktop.tailnet-xyz.ts.net:7317 --token trun_join_7Hk… --name gpu1
```

The join token is exchanged for a long-lived per-agent credential, stored at
`~/.trun/agent.toml` (mode 0600). Later `trun agent` runs reuse it.
`trun agent install-service` sets up systemd, launchd, or a Windows service.

## Transport

- The same MessagePack-framed protocol runs over either an **SSH channel** (SSH mode)
  or **WebSocket** (join mode; TLS unless the link is already a WireGuard tunnel).
- The hub listens only on localhost and, if configured, the tailnet interface. It
  never listens on a public address.
- **Heartbeats** every 15 s in both directions.

## Daemon model

Agents and the hub run as **user-level daemons**, not system services. That means no
admin rights to install, and the processes run as the same user who owns the work.

| OS | Mechanism |
|---|---|
| Linux | `systemd --user` unit, plus `loginctl enable-linger` so it survives logout (important on headless boxes). Without systemd, a detached process with a pidfile |
| Windows | Per-user Scheduled Task at logon, running hidden. The Tauri tray app starts the hub if it isn't running |
| macOS | `launchd` LaunchAgent |

User level covers everything trun needs: NVML GPU stats, process-tree sampling of the
user's own processes, Job Objects and process groups, and disk stats. Only one feature
needs elevation, **host shutdown**, and it uses the narrowest possible path instead of
making the whole daemon privileged:

- Linux: `systemctl poweroff` through polkit (allowed for the active user on most
  distros), otherwise `sudo -n shutdown` if a NOPASSWD rule exists. On cloud boxes
  where the agent already runs as root, it is called directly. `trun doctor` reports
  which path, if any, works.
- Windows: `shutdown /s` works for an interactive user without elevation.
- macOS: needs a sudoers rule; `trun doctor` explains how to add one.

If a future feature truly needs a system service, it gets a tiny separate privileged
helper with a narrow IPC surface. The main daemon stays unprivileged.

## Spool & resume

- Every event gets a per-run monotonically increasing `seq`, assigned by the agent.
- Events are written to the agent's SQLite spool **before** sending.
- The hub acknowledges by `(run_id, seq)`, and acknowledged events become eligible
  for spool pruning.
- On reconnect the agent sends `resume {run_id: last_acked_seq}` per active run and
  replays the gap. The UI marks the gap period as "backfilled".
- Spool limits: a size cap (default 1 GB). When over the cap, the agent drops
  `output` events for the oldest runs first, while **always keeping** metrics,
  lifecycle, alerts, and diagnosis. It records that the drop happened.

## Remote execution

The hub (and therefore the CLI and MCP) can ask an agent to start a run:

```sh
trun run --host gpu1 --cwd ~/proj -- python train.py
```

In **SSH mode** this is on by default, because the hub can already run anything there
through SSH. In **join mode** it is **off by default**: the agent must be enrolled with
`--allow-exec` or have `allow_exec = true` in its config. Optional `exec_allowlist` globs restrict which
commands may be started remotely. Cancel, note, and `::expect` overrides are always
allowed.

## GPU and system telemetry

- **NVIDIA:** NVML gives utilization, memory, temperature, power, clocks, and
  per-process GPU memory, so a run is linked to the GPUs its process tree actually
  uses.
- **AMD:** rocm-smi, planned.
- **Apple Silicon:** powermetrics, best effort, planned.
- **System:** CPU, memory, disk (per mount), network throughput, load.
- The sampling interval defaults to 5 s. For long retention, samples go through the
  same downsampling pipeline as metrics (below).

## Downsampling

High-frequency data (per-batch metrics at hundreds of Hz, 1 s system samples) is
downsampled **on the agent**, before spooling and sending. This keeps the link and hub
storage small no matter how chatty the program is. It is fully user-configurable:

```toml
# .trun/config.toml (project) or ~/.trun/config.toml (global); per-task override in templates
[downsample]
window = "5s"                     # bucket width; or points = 2000 per run for adaptive width
keep_raw = "10m"                  # also keep full-resolution data for the most recent 10 min
aggregates = ["min", "max", "mean", "std", "count", "last", "p10", "p25", "p50", "p75", "p99"]

[downsample.metric."loss"]        # per-metric override
aggregates = ["mean", "min", "last"]
window = "1s"

[downsample.metric."grad_norm"]
aggregates = ["max", "p99", "count_nan"]
```

- The aggregates are `min`, `max`, `mean`, `sum`, `std`, `var`, `count`, `count_nan`,
  `first`, `last`, and any percentile `pNN` or `pNN.N`. Percentiles use a streaming
  sketch (t-digest), so memory per bucket stays bounded.
- NaN and Inf values are never averaged away. They are counted (`count_nan`) and always
  preserved as individual events, because detecting them is the point.
- Checks on the agent see **full-resolution** data for their evaluation window before
  downsampling, so no alert is missed because of bucketing.
- Panels choose which aggregate to plot (`y = "loss.p50"`, a band from `loss.p10` to
  `loss.p99`, and so on). The default is `mean` with a `min`/`max` band.
- Samples use the same mechanism with their own defaults (`mean`, `max` per 1 min
  after the first 24 h).

## Preemption detection

On spot or preemptible instances the agent polls the provider's metadata endpoint
(auto-detected):

| Provider | Signal |
|---|---|
| AWS | `http://169.254.169.254/latest/meta-data/spot/instance-action` (IMDSv2) |
| GCP | `computeMetadata/v1/instance/preempted` |
| Azure | Scheduled Events `Preempt` |
| Others (Lambda, RunPod, Vast, …) | SIGTERM to the agent and the shutdown sequence, as a generic fallback |

On notice the agent:

1. emits a `preempted` warning (on AWS usually about 2 minutes ahead)
2. optionally sends `SIGTERM` to the run so the program can checkpoint
   (`on_preempt = "signal"` in the task template)
3. flushes the spool as far as it can
4. marks the lifecycle `preempted`, not `failed`

## Cost protection

Idle GPUs cost money. The agent can act without the hub:

- The default `GPU idle` check flags a stall after 10 minutes.
- In task templates or agent config, you can opt in to escalation:

```toml
# ~/.trun/agent.toml on the cloud box
allow_shutdown = true

[cost]
hourly_rate = 2.49            # optional; enables "$ spent" in the UI
idle_shutdown_after = "30m"   # no active runs on this host for 30 min → power off
```

- Checks can call `shutdown_host(msg)` only when `allow_shutdown = true`.
- Before powering off, the agent flushes the spool and the hub records why.

## Security summary

| Surface | Default |
|---|---|
| Hub API / UI | Bound to localhost and optionally the tailnet interface. Never public. Non-localhost access (for example your phone over Tailscale) requires a device token |
| Hub ↔ agent | SSH mode: your SSH keys. Join mode: TLS or WireGuard plus a per-agent credential, revocable from the hub |
| Remote exec | SSH mode: on (equivalent to SSH access). Join mode: off unless enabled per agent. Optional command allowlist in both |
| Host shutdown | Off unless `allow_shutdown = true` on that agent |
| Scripts (checks, parsers) | Rhai sandbox, no I/O |
| TS panels | Sandboxed iframe, no network |
| Secrets in output | A redaction filter (configurable regexes, plus common token formats) runs on the agent **before** spooling or sending |
