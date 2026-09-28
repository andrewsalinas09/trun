# 06 · Panels & dashboards

Panels are files. The UI watches them and re-renders on save. A panel with an error
shows the error in place, and the rest of the dashboard keeps working. The error is
also available over MCP (`get_panel_errors`), so an AI agent that writes a panel can
see and fix its own mistakes.

There are two tiers:

1. **TOML panels** are declarative and cover most needs. They can't break the UI and
   are easy to lint.
2. **TypeScript panels** are an escape hatch: arbitrary rendering with Observable Plot
   or the DOM, in a sandboxed iframe.

Nothing is compiled into the binary. Edits appear live.

## Built-in views (no files needed)

Every run automatically gets:

- a header with lifecycle, health, elapsed time and ETA, host, and command
- the step tree with progress bars
- a live log (virtualized, ANSI-colored, filter/grep, jump to sequence number)
- open and cleared alerts, plus the diagnosis
- notes
- a small multiple of **every metric** it has emitted (auto panels)
- host resources over the run's duration (CPU, memory, GPU util/memory/temp)

Panel files are for customizing and combining views beyond that.

## TOML panels

```toml
# .trun/panels/loss.toml
title  = "Training loss"
kind   = "line"                    # line | area | bar | scatter | heatmap | histogram
                                   # | stat | table | gallery | log
source = "metric"                  # metric | progress | sample | alert | artifact
y      = ["loss", "val_loss"]
x      = "step"                    # step | time | elapsed
smooth = 0.9                       # EMA smoothing (display only)
scale.y = "log"
runs   = "current"                 # current | compare | "tag:exp=lr-sweep" | "name:train-*"
```

More examples:

```toml
# Big-number tile
title = "Throughput"
kind = "stat"
source = "metric"
y = "throughput"
reduce = "avg:1m"                  # last | avg:<dur> | max | min
unit = "items/s"
thresholds = [{ below = 500, color = "warn" }]
```

```toml
# GPU memory across all GPUs on the run's host
title = "GPU memory"
kind = "area"
source = "sample"
y = "gpu.mem"
group = "gpu"                      # one series per GPU
x = "elapsed"
```

```toml
# Sample images emitted with ::artifact kind=image
title = "Generated samples"
kind = "gallery"
source = "artifact"
filter = { kind = "image", path = "out/samples_*" }
latest = 8
```

```toml
# Overlay current run against previous runs with the same template
title = "Loss vs previous runs"
kind = "line"
source = "metric"
y = "val_loss"
x = "step"
runs = "compare"                  # current + last 5 runs of same template
```

Internally, a TOML panel compiles to an Observable Plot spec. `trun lint` validates
fields against a JSON Schema, which is published so editors and agents get
autocompletion.

## TypeScript panels

```ts
// .trun/panels/confusion.ts
import type { PanelContext } from "trun:panel";

export default function render({ run, plot, Plot, el }: PanelContext) {
  const m = run.metricLatestObject("confusion_matrix"); // emitted as JSON metric/artifact
  if (!m) return el.text("waiting for confusion matrix…");
  return Plot.plot({
    color: { scheme: "blues", legend: true },
    marks: [Plot.cell(m.cells, { x: "pred", y: "actual", fill: "count", tip: true })],
  });
}

// Optional: re-render policy
export const refresh = { onMetric: ["confusion_matrix"], throttleMs: 2000 };
```

- The file is transpiled in the browser (esbuild-wasm or sucrase) on load. There is
  no build step.
- It runs in a **sandboxed iframe** with no network and no access to the parent page,
  and talks to the host through a message-passing `PanelContext`.
- `PanelContext` provides read-only data access (`run.metric(name)`, `run.steps`,
  `run.logs.tail(n)`, `run.samples(...)`, `runs.compare(...)`) plus `Plot`, a small
  `el` DOM helper, and the theme tokens, so panels match light and dark mode.
- Exceptions and render timeouts are shown in the panel and reported as `panel_error`
  events.

## Dashboards

A dashboard arranges panels:

```toml
# .trun/dashboards/training.toml
title = "Training"
applies = "train*"                 # default dashboard for matching runs
columns = 12

[[row]]
panels = [
  { panel = "builtin:header", span = 12 },
]

[[row]]
panels = [
  { panel = "loss", span = 8 },
  { panel = "throughput", span = 4 },
]

[[row]]
panels = [
  { panel = "gpu-memory", span = 6 },
  { panel = "builtin:alerts", span = 6 },
]

[[row]]
panels = [ { panel = "builtin:log", span = 12, height = 400 } ]
```

There is also a **fleet view** (not per run): every active run on every host, with
health, progress, and a sparkline of each run's key metric. This is the "is anything
on fire?" screen, and the default landing page.

## Where panel files come from

Panels live next to the run's project, which may be on a remote host. Agents
therefore **sync the run's `.trun/` config to the hub** when the run starts and on
every change (it is small text, rate-limited). The hub resolves panels for a run as:

1. the run's project `.trun/`, synced from the agent
2. the hub-side project directory, if the same project is open locally
3. global `~/.trun/` on the hub

An agent writing a panel on the desktop for a remote run can use the hub-side copy
or `write_panel` over MCP. Either way, the result appears live.
