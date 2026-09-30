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

### SSH mode (implemented in M4)

```sh
trun hosts add gpu1                   # uses the "gpu1" entry in ~/.ssh/config
trun hosts add gpu1 --install         # also copies the right trun binary to the host
trun hosts add pi --ssh "wsl ssh"     # use another ssh client (here: WSL's, with its config and keys)
trun hosts                            # online/offline, platform, clock skew, active runs
trun run --host gpu1 --cwd ~/proj -- python train.py
```

Every host runs the **same trun daemon** as the desktop: its own supervisor, checks,
and SQLite store. It works fully without the hub. The hub keeps one link per host:

1. `ssh <target> <trun_path> hub ensure --json` starts the remote daemon if needed
   (upgrading it if the installed binary changed) and prints its loopback port and
   token.
2. `ssh -N -L 127.0.0.1:<free local port>:127.0.0.1:<remote port>` forwards that
   port. The remote daemon still only listens on its own loopback. The tunnel
   requests carry the remote port in `Host`, so the daemon's Host check passes.
3. The hub measures the clock offset (`/api/health` `now`, midpoint of the round
   trip) and shifts all remote timestamps onto the hub's clock. The Pi used in
   testing ran two minutes ahead.
4. It subscribes to the remote fleet stream, then reconciles the recent run list,
   and **mirrors** each remote run into the local agent and store. Run ids and event
   `seq` numbers are kept, so a mirrored run looks like a local run to the CLI, UI,
   `trun wait`, and (later) MCP. Its `via` field names the host.
5. Commands for a mirrored run (start, cancel) are forwarded to the host's API.
   Panels are read on the host, where the project files live: the run's panel
   stream is proxied.

ssh runs with `BatchMode=yes` (it never prompts) and a connect timeout. Errors are
translated into a next step: "permission denied" (keys), "could not resolve" (try
`--ssh "wsl ssh"` when the config lives in WSL), and "trun not found" (`--install`).
The link retries with backoff. A request that arrives while the link is still
connecting waits up to 20 s instead of failing.

`--install` runs `uname -sm` on the host, picks the matching static binary
(`$TRUN_HOME/dist/trun-<triple>`, or `--binary`), and uploads it over the same ssh
command's stdin with an atomic rename. No scp or sftp is needed.

When a link drops, mirrored active runs get a hub-side `stalled` alert ("host 'pi'
unreachable: …"), so `trun wait --until stalled` wakes up. The run itself keeps
going on the host. After reconnecting, the alert clears and the missed events are
backfilled.

### Join mode (planned)

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

- **SSH mode:** the host daemon's own HTTP + SSE API (the one the local CLI and UI
  use) runs through an ssh port forward. No second protocol exists, and anything the
  hub can do locally works remotely. The ssh process's liveness is the heartbeat;
  SSE keep-alives run every 15 s.
- **Join mode (planned):** the same API over an outbound WebSocket (TLS unless the
  link is already a WireGuard tunnel).
- The hub listens only on localhost and, if configured, the tailnet interface. It
  never listens on a public address.

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

- Every event gets a per-run monotonically increasing `seq`, assigned by the host's
  agent.
- The host's own SQLite store is the spool. It is written before anything is served,
  so nothing depends on the link being up.
- The hub replicates by pulling: for each remote run it streams
  `/runs/<id>/events?since_seq=<highest seq stored locally>`. After a reconnect it
  resumes from there, so the gap is backfilled exactly once. Duplicates are dropped
  by `seq`.
- Summaries (lifecycle, steps, health) arrive on the fleet stream and can lag the
  event stream. So the mirror applies lifecycle and diagnosis events to the summary
  itself, and a lagging summary never un-finishes a run. A summary's `last_seq` is
  the event its content is consistent with. Clients resume streams from there and
  drop what they already have.
- Planned: a spool size cap (default 1 GB). When over the cap, drop `output` events
  for the oldest runs first, while **always keeping** metrics, lifecycle, alerts,
  and diagnosis, and record that the drop happened. Also planned: a "backfilled"
  marker in the UI.

## Remote execution

The hub (and therefore the CLI and MCP) can ask an agent to start a run:

```sh
trun run --host gpu1 --cwd ~/proj -- python train.py
```

`--cwd` is a path on the host. Relative paths and `~` are resolved from the host
user's home (default: the home directory). The run's `.trun/` config, checks, and
panels come from the host's copy of the project. In **SSH mode** this is on by
default, because the hub can already run anything there through SSH. In **join mode** it is **off by default**: the agent must be enrolled with
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
| Scripts (checks, parsers) | Starlark: hermetic by design (no I/O primitives exist), cancellation budget |
| TS panels | Sandboxed iframe, no network |
| Secrets in output | A redaction filter (configurable regexes, plus common token formats) runs on the agent **before** spooling or sending |
