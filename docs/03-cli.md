# 03 · CLI

All commands accept `--hub <url>` (default: the local daemon) and `--json` for
machine-readable output. A `RUN` argument accepts a run ID, a unique ID prefix, or a
run name. A name resolves to the most recent run with that name.

## Running things

```sh
trun run [OPTIONS] -- <cmd> [args...]
trun run [OPTIONS] <template>             # from .trun/tasks/<template>.toml
```

| Option | Meaning |
|---|---|
| `--name <n>` | Human name (default: derived from the command) |
| `--host <h>` | Run on a remote agent instead of locally (the agent must allow remote exec) |
| `--cwd <dir>` | Working directory |
| `--env K=V` | Extra environment (repeatable) |
| `--pty` | Run under a pseudo-terminal |
| `--detach`, `-d` | Return right away and print the run ID (default: stream output and exit with the command's code) |
| `--check <file>` | Attach extra check scripts (repeatable) |
| `--no-default-checks` | Disable global default checks |
| `--tag k=v` | Metadata for filtering and comparison |

While attached, `trun run` streams output as usual, so it can be a drop-in prefix
for existing commands. Ctrl-C cancels the run. `Ctrl-\` detaches without cancelling.

## Inspecting

```sh
trun ls [--host h] [--active] [--name glob] [--since 1d]
trun status RUN                 # digest: lifecycle, health, step tree, metrics, alerts, tail
trun logs RUN [-f] [--grep re] [--since-seq N] [--tail N] [--stream stdout|stderr]
trun metrics RUN [name...] [--range 10m]
trun hosts                      # agents, online/offline, GPU summary
trun diff RUN_A RUN_B           # compare duration, steps, final metrics, diagnosis
```

## Waiting (the wake-up primitive)

```sh
trun wait RUN --until done,failed,stalled,lost,preempted,alert [--timeout 2h] [--quiet]
```

This blocks until any listed condition is true, then prints a digest.

| Exit code | Meaning |
|---|---|
| 0 | Run succeeded |
| 1 | Run failed or was cancelled |
| 2 | Health became stalled |
| 3 | Lost or preempted |
| 4 | Timeout reached |
| 5 | An alert fired (with `--until alert`) |
| 10 | Usage or connection error |

It is designed for AI agents: run `trun wait` as a background command, and the harness
wakes the agent when it exits. No polling. If the condition is already true, it returns
right away.

## Controlling

```sh
trun cancel RUN [--force]
trun note RUN "text"            # attach a note (human -> agent, or agent -> human)
trun rerun RUN                  # same cmd/cwd/env/template
```

## Emitting from scripts

This is an alternative to printing `::` lines, and useful in shell scripts. It needs
`TRUN_RUN_ID` to be set, which the supervisor does automatically.

```sh
trun emit step-begin compile "Compile firmware"
trun emit progress epoch 12 50
trun emit metric loss=0.31 val_loss=0.40 --step 1200
trun emit artifact samples/ep12.png --kind image
trun emit step-end compile ok
```

## Daemons

```sh
trun hub [--listen 0.0.0.0:7317] [--data-dir …]
trun hosts add <ssh-host> [--install]                 # SSH mode: hub dials the host
trun hosts remove <host>
trun hub token create --name gpu1 [--ttl 1h] [--allow-exec]   # join mode
trun hub token list | revoke <id>

trun agent --join wss://hub:7317 --token <join-token>    # first time: enroll
trun agent                                             # subsequent: use stored credential
trun agent --stdio                                     # used by the hub over SSH; not run by hand
trun agent install-service                             # user-level: systemd --user / scheduled task / LaunchAgent
trun doctor                                            # check daemon, GPU access, shutdown permission, connectivity
```

## Authoring helpers (for humans and AI)

```sh
trun lint [path]                # validate .trun/ panels, checks, parsers, templates
trun check test <file> --run RUN   # replay a check against a recorded run's data
trun parse test <file> < sample.log # show which events a parser emits for input
trun ui                         # open the UI (Tauri app if installed, else browser)
trun mcp                        # stdio MCP server
trun init claude [--project]    # register MCP, add CLAUDE.md rule, install PreToolUse hook
trun hook claude-pretool        # the hook itself (reads hook JSON on stdin)
```

`trun check test` and `trun parse test` exist so an agent can iterate on a check or
parser against real recorded data without waiting for a live run to hit the condition.
