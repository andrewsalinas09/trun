# 08 · MCP interface

`trun mcp` is a stdio MCP server. It is a thin client of the hub API, so an AI agent
on the desktop sees every run on every host.

Register it in Claude Code:

```sh
claude mcp add trun -- trun mcp
```

## Design rules

1. **Digests, not dumps.** Status responses are compact and summarize state within a
   token budget. Raw logs are available on request, with filters and cursors.
2. **Cursors everywhere.** Log and event reads take `since_seq`, so repeated checks
   only return what's new.
3. **Never force polling.** The wake-up pattern (below) uses `trun wait` as a
   background command. MCP is for inspecting and acting, not for waiting.
4. **Files are the configuration API.** Panels, checks, parsers, and templates are
   files. The agent can edit them with its normal file tools. The `write_*` tools
   exist for remote projects and to get validation feedback in one step.
5. **Every error is readable.** Check errors, panel errors, and parser errors are all
   queryable, so the agent can close its own loop.

## Tools

### Runs

| Tool | Params | Returns |
|---|---|---|
| `start_run` | `cmd`, `name?`, `host?`, `cwd?`, `env?`, `template?`, `pty?`, `tags?` | `run_id` plus the command to wait on |
| `list_runs` | `active?`, `host?`, `name?`, `since?`, `limit?` | One line per run: id, name, host, lifecycle, health, progress %, age |
| `get_status` | `run` | **Digest** (see below) |
| `tail_logs` | `run`, `since_seq?`, `lines?` (default 100), `grep?`, `stream?` | Lines with seq numbers, plus `next_seq` |
| `query_metric` | `run`, `names[]`, `range?`, `points?` (downsampled, default 50) | Series plus summary stats (last/min/max/slope) |
| `compare_runs` | `runs[]` | Duration, steps, final metrics, diagnosis, side by side |
| `cancel_run` | `run`, `reason`, `force?` | Final lifecycle |
| `rerun` | `run`, `overrides?` | New `run_id` |

### Human ↔ agent

| Tool | Params | Returns |
|---|---|---|
| `read_notes` | `run?`, `unread_only?` (default true) | Notes from the human. Marks them read |
| `add_note` | `run`, `text` | Shown to the human prominently in the UI (for example "I restarted with lr=1e-4 because of the plateau") |

### Structure and configuration

| Tool | Params | Returns |
|---|---|---|
| `define_steps` | `run`, `steps` (tree) | Pre-declares the expected step tree so progress shows before the program emits anything |
| `write_panel` / `write_check` / `write_parser` | `scope` (`project:<path>` \| `global` \| `run:<id>`), `name`, `content` | Lint result. Once live, the first render or evaluation result |
| `get_errors` | `run?`, `kind?` (`check` \| `panel` \| `parser`) | Current errors with file, line, and message |
| `test_check` | `file` or `content`, `run` | Replay timeline of when alerts would open and clear |

### Fleet

| Tool | Params | Returns |
|---|---|---|
| `list_hosts` | none | Hosts, online status, GPUs, active run count, cost rate |

## The digest

`get_status` returns a compact, predictable summary, around 40 lines or fewer:

```
run 01J9Z… "train-v3" on gpu1 · running 1h12m · health: STALLED
cmd: python train.py --epochs 50        cwd: ~/proj
diagnosis: –
alerts (open):
  [stalled] GPU idle for 5 min (dataloader bottleneck or hang?)   since 4m
  [warn]    loss has plateaued for 15 min                          since 11m
steps:
  ✓ setup            12s
  ▸ train            epoch 23/50  (46%, eta 1h21m, rate slowing ↓)
      ▸ epoch-23     batch 118/400
metrics (last · trend 15m):
  loss      0.4121  flat
  val_loss  0.4630  flat
  lr        3.0e-4  –
host: gpu util 2% · gpu mem 71/80 GB · cpu 98% (1 core) · disk free 41 GB
last output 3m ago (seq 18422):
  [18418] Epoch 23: 29%|██▉      | 118/400 [02:11<05:12, 0.90it/s]
  ...
unread human notes: 1  → read_notes
```

The digest is also exactly what `trun wait` prints when it returns, and what
`trun status` prints. That keeps all three consistent.

## The wake-up pattern (Claude Code)

MCP calls are request/response, so the agent can't be pushed events over MCP. Instead:

1. `start_run` (or `trun run -d …` in a shell), which returns a run ID.
2. Run `trun wait <id> --until done,failed,stalled,lost,preempted` **as a background
   command**.
3. Carry on with other work. When `trun wait` exits, the harness wakes the agent with
   the digest as output, and the exit code says which condition fired.
4. React: inspect (`tail_logs`, `query_metric`), fix, `cancel_run`, `rerun`, or
   `add_note` for the human.

This turns "remember to check on it later" into an interrupt. `start_run` returns the
exact `trun wait` command line to use.

## Resources (optional)

- `trun://runs/{id}/digest` is the live digest.
- `trun://runs/{id}/logs` is the full log (large, fetched explicitly).

For clients that support resource subscriptions, the digest resource sends change
notifications. That is an additional push path, used where available.
