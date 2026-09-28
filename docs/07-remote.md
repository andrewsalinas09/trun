# 07 · Remote & cloud

The flagship remote scenario is a training job on a rented GPU box, watched from the
desktop and from an AI agent. The same mechanism covers a Pi, a CI runner, or a
second workstation.

## Principles

1. **Agents dial out.** Remote hosts never open inbound ports. They are often behind
   NAT and ephemeral.
2. **The edge is autonomous.** Capture, checks, and cost-protection actions run on the
   agent and don't depend on the connection.
3. **Nothing is lost.** Events are spooled locally and replayed after reconnecting.
4. **Setup is one line.** A static binary plus a join token.

## Joining a host

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

- **WebSocket over TLS**, MessagePack frames.
- **Reachability:**
  - **v1: Tailscale / WireGuard.** The hub listens on its tailnet address, and agents
    connect to it. This works through NAT with no public exposure. Tailscale's
    ephemeral auth keys suit cloud boxes well.
  - **Later: relay.** A small Cloudflare Worker plus Durable Object that both hub and
    agents dial out to. It forwards opaque, end-to-end encrypted frames and gives a
    public URL with no VPN. See the open questions.
  - **Direct:** a hub on a public address with its own TLS certificate also works.
- **Heartbeats** every 15 s in both directions.

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

This is **off by default**. The agent must be enrolled with `--allow-exec` or have
`allow_exec = true` in its config. Optional `exec_allowlist` globs restrict which
commands may be started remotely. Cancel, note, and `::expect` overrides are always
allowed.

## GPU and system telemetry

- **NVIDIA:** NVML gives utilization, memory, temperature, power, clocks, and
  per-process GPU memory, so a run is linked to the GPUs its process tree actually
  uses.
- **AMD:** rocm-smi, planned.
- **Apple Silicon:** powermetrics, best effort, planned.
- **System:** CPU, memory, disk (per mount), network throughput, load.
- The sampling interval defaults to 5 s. Samples are downsampled on the hub for long
  retention (raw for 24 h, then 1-minute rollups).

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
| Hub API / UI | Bound to localhost and the tailnet interface. Remote UI access requires a user token |
| Agent → hub | TLS plus a per-agent credential. Revocable from the hub |
| Remote exec | Off unless explicitly enabled per agent. Optional command allowlist |
| Host shutdown | Off unless `allow_shutdown = true` on that agent |
| Scripts (checks, parsers) | Rhai sandbox, no I/O |
| TS panels | Sandboxed iframe, no network |
| Secrets in output | A redaction filter (configurable regexes, plus common token formats) runs on the agent **before** spooling or sending |
