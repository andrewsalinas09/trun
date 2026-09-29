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
{"kind":"metric","values":{"loss":0.31,"grad":"NaN"},"step":1200}
{"kind":"progress","id":"train","current":12,"total":50,"unit":"epoch"}
{"kind":"step_begin","id":"eval","name":"Evaluate","parent":null}
{"kind":"step_end","id":"eval","status":"ok"}
{"kind":"log","level":"warn","text":"disk nearly full"}
{"kind":"note","text":"switching to schedule B"}
{"kind":"heartbeat"}
{"kind":"expect","silence_ms":1200000}
```

JSON has no literal for NaN or infinity, so metric values may also be the strings
`"NaN"`, `"inf"`, or `"-inf"`. They are stored and shown, never dropped. The side
channel also accepts `::` protocol lines, which is how `trun emit` works.

Endpoints: on Unix, a socket in a per-user 0700 directory
(`$TMPDIR/trun-<uid>/<run>.sock`). On Windows, the named pipe
`\\.\pipe\trun-<run id>`, whose default ACL admits only the owner, and remote
clients are rejected.

**Python helper:** [`sdk/python/trun.py`](../sdk/python/trun.py) is a single file
using only the standard library: `trun.step()`, `progress()`, `metric()`, `note()`,
`warn()`, `heartbeat()`, `expect_silence()`. Outside trun every call is a no-op, and
a broken channel never raises into the program. Rust and TypeScript helpers are
planned, but optional, because the format is trivial.

## Built-in parsers

These always run on every line. Each one only matches its own tool's output, so
they don't need to be switched on. Per-task opt-out comes with task templates.

| Parser | Status | Recognizes | Emits |
|---|---|---|---|
| `tqdm` | ✅ M2 | tqdm bars with or without a total, `unit_scale` suffixes (`1.2M`), `s/it` rates, redrawn with `\r` | a step per bar description, progress, and the postfix (`loss=0.12`) as metrics |
| `pytest` | ✅ M2 | activates on `=== test session starts ===`: collection count (including deselected), default and `-v` result lines, xdist, the final tally | a `tests` step with progress, `FAILED` lines as errors, and `tests_passed`/`tests_failed`/… metrics |
| `cargo` | ✅ M2 | `Compiling`/`Checking`, the TTY `Building [..] n/m` bar, `Finished`, `error: could not compile`, a test binary per `Running`/`Doc-tests`, `test … ok/FAILED`, `test result:` | a `build` step with crate progress, a step per test binary with per-test progress, failures, and cumulative test counts |
| `generic-fraction` | ✅ M2 | `N/M`, `[N/M]`, `N of M` at line start (`Epoch 3/10`, `[12/50] compiling`) | progress. Low confidence: only used while nothing more specific has reported progress for the run |
| `jest`/`vitest` | planned | suite and test results | progress, failures |
| `hf-trainer` | planned | HuggingFace Trainer log dicts | metrics (loss, lr, epoch) |
| `lightning`, `keras` | planned | their progress and metric lines | progress, metrics |
| `diagnosis` | ✅ M1 | tracebacks, `CUDA out of memory`, `Killed`, segfaults, `ModuleNotFoundError`, `No space left on device`, … | a diagnosis event with a classified cause |

Built-in parsers see the line with ANSI escapes removed, including `\r`
redraws (provisional lines), so a bar is tracked while it moves, not only when it
finishes.

### How structure becomes state

The agent folds directives into the run summary (`steps`, `metrics`):

- Progress without an `id` goes to the most recently begun open step, or to an
  implicit `progress` step.
- A step whose `current` reaches `total` completes automatically (`ok`). If
  progress moves backwards (a bar restarts), the step reopens and its rate resets.
- Ending a step ends its running children with the same status. When the run ends,
  open steps close as `ok` (succeeded), `skipped` (cancelled), or `failed`.
- Rate is an exponential moving average of units per second. ETA is
  `(total − current) / rate`.
- Progress updates are coalesced into at most one `progress` event per step every
  250 ms. The summary always has the latest value.
- The summary keeps at most 256 steps and 200 metric names. All metric points are
  stored regardless.

The diagnosis parser always runs. Its output feeds the run's final `diagnosis` field,
so "why did it fail?" has a one-line answer.

## Custom parsers (Starlark)

`.trun/parsers/*.star` files are loaded and hot-reloaded. A parser declares which runs
it applies to and defines a function called on each line:

```python
# .trun/parsers/my-sim.star
META = {"applies": "sim-*", "priority": 10}

SIM = regex(r"^t=(\d+) energy=([-\d.e]+) residual=([-\d.e]+)")  # compiled once, native Rust regex

# Called for each output line. Return None for no events, or an event dict / list of dicts.
def parse(line, stream):
    m = SIM.match(line)
    if m:
        return {
            "kind": "metric",
            "step": int(m[1]),
            "values": {"energy": float(m[2]), "residual": float(m[3])},
        }
```

Regexes are compiled once at load time by the host (the Rust `regex` crate), so
per-line matching runs at native speed. Parsers must be fast. A per-line time budget applies (default 200 µs), and a parser
that repeatedly exceeds it is disabled for the run with a `check_error` event. Use
`trun parse test` to develop against captured logs.
