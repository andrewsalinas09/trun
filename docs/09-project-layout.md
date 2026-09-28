# 09 · Project layout & config

## `.trun/` in a project

Commit this directory. The repository then carries its own monitoring: how to run
things, what "healthy" means, and how to view them.

```
my-project/
  .trun/
    config.toml            # project defaults; `name = "detector"` sets the project identity
    tasks/                 # task templates:  trun run <name>
      train.toml
      test.toml
    checks/                # Starlark checks (.star)
      training.star
    parsers/               # Starlark parsers (.star)
      my-sim.star
    panels/                # TOML / TS panels
      loss.toml
      confusion.ts
    dashboards/
      training.toml
```

## Task templates

A template bundles the command with its expected structure, checks, and views, so that
`trun run train` gives a fully instrumented run every time.

```toml
# .trun/tasks/train.toml
name    = "train"
cmd     = ["python", "train.py", "--config", "configs/base.yaml"]
cwd     = "."
env     = { WANDB_MODE = "offline" }
host    = "gpu1"                   # optional default host
pty     = false
tags    = { project = "detector" }

max_duration = "12h"               # hub-side wall clock limit
on_preempt   = "signal"            # signal | nothing
dashboard    = "training"

checks  = ["training"]             # in addition to defaults
parsers = ["hf-trainer"]           # enable specific built-ins; others auto-detect

# Expected step tree: visible before the program emits anything
[[steps]]
id = "setup"
name = "Setup & data loading"

[[steps]]
id = "train"
name = "Train"
total = 50
unit = "epoch"

[[steps]]
id = "eval"
name = "Final eval"

[args]                              # parameters: trun run train --set epochs=10
epochs = { default = 50, flag = "--epochs" }
lr     = { default = "3e-4", flag = "--lr" }
```

## Global config: `~/.trun/`

```
~/.trun/
  config.toml        # hub/agent/client settings, default check thresholds
  agent.toml         # agent credential + permissions (allow_exec, allow_shutdown, cost)
  checks/defaults.star
  panels/            # global panels usable from any run
  dashboards/
  data/              # hub role: hub.db + projects/<name>.db + artifacts/ (see 02-architecture.md)
  spool/             # agent spool (agent role)
```

```toml
# ~/.trun/config.toml
[hub]
listen = ["127.0.0.1:7317", "tailnet:7317"]
retention.logs = "30d"
retention.metrics = "1y"
retention.samples_raw = "24h"

[client]
hub = "ws://127.0.0.1:7317"

[defaults.checks]
silence = "5m"
no_progress = "15m"
gpu_idle = "10m"
disk_warn = "2GB"

[downsample]                          # see 07-remote.md#downsampling
window = "5s"
keep_raw = "10m"
aggregates = ["min", "max", "mean", "std", "last", "p10", "p50", "p99"]

[notify]
desktop = true
ntfy = { topic = "trun-alerts" }     # phone push via ntfy.sh (optional)
on = ["failed", "stalled", "preempted", "succeeded:>30m"]  # succeeded only if run was long

[redact]
patterns = ["(?i)api[_-]?key\\s*[:=]\\s*\\S+"]
```

## Resolution order

Checks, parsers, panels, and dashboards with the same name resolve with this
precedence (first wins):

1. `run:<id>` scope (written through MCP for one specific run)
2. project `.trun/` (synced from the owning agent)
3. hub-side copy of the project, if it exists
4. global `~/.trun/`
5. built-ins

Default checks are **additive** unless overridden by name or disabled with
`--no-default-checks`, or with `default_checks = false` in the template.

## Hot reload

All files under `.trun/` and `~/.trun/` are watched.

- **Checks and parsers** reload on the agent that uses them. A reload that fails to
  compile keeps the previous version running and emits a `check_error`.
- **Panels and dashboards** re-render in all open UIs.
- **Templates** apply to the next run only.
