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

## Implementation status (M2)

| Feature | Status |
|---|---|
| TOML panels, validated strictly (unknown keys are errors, reported per file with line and column) | ✅ |
| Kinds: `line`, `area`, `scatter`, `bar` (latest value per metric), `stat` (with `reduce` and `thresholds`), `table` | ✅ |
| `heatmap`, `histogram`, `gallery`, `log` kinds | planned |
| `source = "metric"` | ✅ |
| Other sources (`progress`, `sample`, `alert`, `artifact`) | planned (M3/M4/M7) |
| `x` = `step` / `time` / `elapsed`, `smooth`, `scale.y` = `log` | ✅ |
| `runs = "current"` | ✅ |
| Comparing runs | planned (M6) |
| Dashboards: `applies` glob, 12-column rows, `span`, `height`, builtins `builtin:{diagnosis,steps,metrics,log}` | ✅ |
| Hot reload: debounced file watcher → SSE `/api/runs/:id/panels/stream`. Measured latency is about 100 ms | ✅ |
| TypeScript panels | planned (M6). A `.ts` file is listed with an explanatory error |
| NaN and inf values drawn as red dashed rules on charts | ✅ |

A worked example is in [`examples/training/.trun/`](../examples/training/.trun).

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

- There is no build step for the user. The **hub compiles panels natively with
  [oxc](https://oxc.rs)**, a Rust TypeScript/TSX parser, transformer, and linter,
  embedded in the trun binary. Compiling takes about a millisecond per file, and
  nothing heavy is shipped to the browser, so it works the same in Tauri, a desktop
  browser, or a phone.
- Compile errors are reported with file, line, and column, both in the panel and
  through `get_errors`, so an AI can fix its own panel.
- **Type checking.** oxc strips types but does not check them. trun ships a
  `trun-panel.d.ts` for the `PanelContext` API, which gives autocomplete and types in
  any editor. If `tsgo` (the native TypeScript compiler) or `tsc` is on PATH,
  `trun lint` and `write_panel` also run a full type check and report type errors.
  Without it, panels still work, and the full check becomes available once it is
  installed.
- Imports: `trun:panel` (types and helpers) and a vendored `@observablehq/plot` are
  resolved by the hub. Relative imports between panel files are bundled by oxc.
  Network imports are not allowed.
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
