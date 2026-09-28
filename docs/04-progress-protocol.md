# 04 · Progress protocol

Structure reaches trun in three ways, from least to most effort:

1. **Built-in parsers** recognize common tools' output automatically.
2. **`::` lines** printed to stdout or stderr from any language, with no library.
3. **The side channel** (`TRUN_EVENTS`) for programs that want clean stdout, or that
   emit high-frequency metrics.

All three produce the same events.

## `::` lines

A line whose first non-whitespace characters are `::` followed by a known verb is
parsed as an event. By default it is also hidden from the displayed log. Unknown verbs
pass through as normal output. To avoid clashes with tools that already use `::`
(for example GitHub Actions), a fully qualified `::trun::verb` form is also accepted,
and strict mode (`--strict-protocol`) accepts only that form.

Arguments are `key=value` pairs. Values may be double-quoted. Bare positional forms
exist for the common verbs.

### Steps

```
::step-begin id=train name="Train model"
::step-begin id=epoch-3 parent=train name="Epoch 3"
::step-end id=epoch-3 status=ok            # ok | failed | skipped
```

Steps form a tree. Ending a parent ends any open children with the same status.
`::step-end` without `id` ends the most recently begun open step.

### Progress

```
::progress id=train current=12 total=50 unit=epoch
::progress train 12/50                     # positional form
::progress 0.42                            # fraction for the current step
```

A progress update for an ID with no matching step implicitly creates that step. trun
computes the rate and ETA itself.

### Metrics

```
::metric loss=0.3121 val_loss=0.402 lr=3e-4 step=1200
::metric throughput=1830 unit=items/s
```

`step` is the x-axis key when present. Otherwise the timestamp is used. Values are
f64. `nan` and `inf` are accepted and recorded, since detecting them is the point.

### Logs, notes, artifacts

```
::warn Validation set is smaller than expected
::error Checkpoint write failed, retrying
::note Switching to lr schedule B        # shown prominently, included in digests
::artifact path=out/samples_ep12.png kind=image
::artifact path=ckpt/ep12.pt kind=file upload=false
```

Artifacts with `upload=true` (the default for `kind=image` under 5 MB) are shipped to
the hub. Otherwise only metadata is recorded.

### Heartbeat and expectations

```
::heartbeat                                # "I'm alive" without producing log noise
::expect silence=20m                       # tell the default silence check this phase is legitimately quiet
::expect silence=default
```

`::expect` lets a program say "the next 20 minutes will be quiet (compiling CUDA
kernels)" so the default stall check doesn't fire falsely.

## Side channel

The supervisor sets `TRUN_EVENTS` to a local endpoint: a Unix socket path, or a
named pipe on Windows. Writing newline-delimited JSON there emits events without
touching stdout:

```json
{"kind":"metric","values":{"loss":0.31},"step":1200}
{"kind":"progress","id":"train","current":12,"total":50}
```

Tiny helper libraries (Python, Rust, TypeScript) wrap this, but they are optional,
because the format is trivial. If `TRUN_EVENTS` is unset (running outside trun), the
helpers do nothing.

## Built-in parsers

These are enabled automatically based on the command and output patterns, and can be
turned off per task.

| Parser | Recognizes | Emits |
|---|---|---|
| `tqdm` | tqdm/rich progress bars (including `\r` redraws) | progress, with rate and ETA |
| `generic-fraction` | `N/M`, `[N/M]`, `N of M`, `NN%` at line start | progress (low confidence, used only when nothing better exists) |
| `cargo` | `cargo build`/`test` output | steps per crate, per-test results, failures |
| `pytest` | collection count, `PASSED`/`FAILED`, summary | progress, per-test results, failures |
| `jest`/`vitest` | suite and test results | progress, failures |
| `hf-trainer` | HuggingFace Trainer log dicts | metrics (loss, lr, epoch) |
| `lightning` | PyTorch Lightning progress and metrics | progress, metrics |
| `keras` | `Epoch n/N` and `loss: …` lines | progress, metrics |
| `diagnosis` | tracebacks, `CUDA out of memory`, `Killed`, segfaults, `ModuleNotFoundError`, `No space left on device` | a diagnosis event with a classified cause |

The diagnosis parser always runs. Its output feeds the run's final `diagnosis` field,
so "why did it fail?" has a one-line answer.

## Custom parsers (Rhai)

`.trun/parsers/*.rhai` files are loaded and hot-reloaded. A parser declares which runs
it applies to and a function per line:

```rust
// .trun/parsers/my-sim.rhai
fn meta() {
    #{ applies: "sim-*", priority: 10 }
}

// Called for each output line. Return () for no events, or an event map / array of maps.
fn parse(line, stream) {
    let m = line.match(`^t=(\d+) energy=([-\d.e]+) residual=([-\d.e]+)`);
    if m != () {
        return #{ kind: "metric", step: m[1].parse_int(),
                  values: #{ energy: m[2].parse_float(), residual: m[3].parse_float() } };
    }
}
```

Parsers must be fast. A per-line time budget applies (default 200 µs), and a parser
that repeatedly exceeds it is disabled for the run with a `check_error` event. Use
`trun parse test` to develop against captured logs.
