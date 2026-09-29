# Decisions log

Short records of decisions made during design. Each gives the context, the decision,
and the consequences. Supersede an entry with a new one rather than editing it.

## D1 · Supervision is the foundation, not instrumentation (2026-09-28)

**Context:** The failures that matter most (crash on start, hang, never really started)
are the ones where a program never reports its own progress.
**Decision:** trun owns the process. Output, exit code, resources, and default checks
work with zero cooperation from the program. Progress and metrics are layered on top.
**Consequences:** `trun run -- cmd` must be a drop-in prefix. Capture has to be robust
(pty, `\r`, unbuffering).

## D2 · Headless daemon first; GUI is a client (2026-09-28)

**Context:** Viewers include a desktop app, a browser, a phone, a CLI, and an AI agent.
**Decision:** A hub daemon exposes the HTTP/WS API and serves the web UI. Tauri is
only a shell around the same UI.
**Consequences:** Runs outlive terminals and AI sessions. Every feature is reachable
without the desktop app.

## D3 · Single Rust binary for all roles (2026-09-28)

**Context:** Remote setup on ephemeral cloud boxes must be trivial.
**Decision:** One static binary (musl on Linux) with the subcommands `run`, `agent`,
`hub`, `mcp`, `wait`, `emit`, and so on.
**Consequences:** `curl | sh` install. The UI assets are embedded in the binary.

## D4 · Agents dial out; edge is autonomous (2026-09-28)

**Context:** Cloud GPU boxes are behind NAT, ephemeral, and costly, and connectivity
is flaky.
**Decision:** Agents initiate outbound WSS. Checks and capture run on the agent, with
a local SQLite spool and sequence-numbered resume.
**Consequences:** No inbound ports. Alerts and cost protection work while
disconnected. v1 reachability is through Tailscale, with a relay later.

## D5 · Everything configurable is a file, hot-reloaded (2026-09-28)

**Context:** AI agents are very good at editing files and much worse at complex,
stateful APIs. Humans want to version their monitoring alongside their code.
**Decision:** Panels, checks, parsers, dashboards, and task templates are files in
`.trun/` (project) and `~/.trun/` (global), watched and hot-reloaded. Nothing is
compiled into the binary.
**Consequences:** A fast edit→see loop. Errors must be surfaced per file (UI plus MCP
`get_errors`). A lint command and JSON Schemas are needed.

## D6 · Rhai for checks and parsers (2026-09-28)

**Context:** Checks must run headless on the agent, sandboxed, and be easy for AI to
write.
**Decision:** Rhai, embedded in the agent, with a time-series API and action functions.
There is no separate custom DSL.
**Consequences:** One scripting language on the agent side, with no runtime to
install. Operation and memory limits are needed. Performance is to be revisited (see
the open questions).

## D7 · Panels: TOML → Observable Plot, TS escape hatch (2026-09-28)

**Context:** Most plots are simple. A few need full control.
**Decision:** Declarative TOML panels compile to Observable Plot. TypeScript panels
run in a sandboxed iframe with a message-passing data API and in-browser
transpilation.
**Consequences:** TOML panels can't break the UI. TS panels need sandboxing and error
reporting.

## D8 · Lifecycle and health are separate axes (2026-09-28)

**Context:** "Running but stalled" and "exited but was actually preempted" are
ambiguous as a single state.
**Decision:** `lifecycle` (queued…succeeded/failed/cancelled/lost/preempted) and
`health` (ok/warn/stalled/failing) are tracked independently.
**Consequences:** Clearer UI and digests. `trun wait` can target either axis.

## D9 · AI wake-up via blocking `trun wait`, not MCP polling (2026-09-28)

**Context:** MCP is request/response. Agents forget to check back or waste tokens
polling.
**Decision:** `trun wait --until …` blocks and exits with a condition-specific code
and a digest. Agents run it as a background command and are woken on exit.
**Consequences:** The digest format is shared by `status`, `wait`, and MCP
`get_status`.

## D10 · Local and remote from day one (2026-09-28)

**Context:** The GPU-cloud use case is central, not an add-on.
**Decision:** The protocol, sequence numbering, and config sync are designed for
remote hosts from M1, even though the remote agent ships in M4.
**Consequences:** The local hub uses the same agent code path as remote agents
(an embedded agent).

## D11 · Self-hosted, single user; SSH and tailnet only (2026-09-28)

**Context:** trun is for one person watching their own machines.
**Decision:** No relay service, no multi-user auth, no public exposure. There are two
ways to connect a host. **SSH mode** (default): the hub dials the host over the user's
SSH config and runs `trun agent --stdio`. **Join mode**: the agent dials the hub over a
tailnet or LAN. Supersedes the relay idea in D4. Edge autonomy and spool/resume from
D4 still apply.
**Consequences:** SSH keys are the auth for most hosts. Remote exec is on by default
in SSH mode and opt-in for join mode. `trun hosts add --install` bootstraps a box.

## D12 · Agent-side, user-configurable downsampling (2026-09-28)

**Context:** Per-batch metrics can arrive at hundreds of Hz. Which summary matters
depends on the user and the metric.
**Decision:** The agent downsamples before spooling and sending. Aggregates are chosen
per project, task, or metric from min, max, mean, sum, std, var, count, count_nan,
first, last, and any percentile (t-digest). A recent raw window is kept. NaN and Inf
are never averaged away. Checks evaluate on full-resolution data.
**Consequences:** Link and storage cost are bounded regardless of emit rate. Panels
address aggregates as `metric.agg`.

## D13 · SQLite for everything, including logs (2026-09-28)

**Context:** One store is simpler to operate, back up, and query.
**Decision:** SQLite (WAL) for hub history, logs, metrics, samples, and the agent
spool.
**Consequences:** No second storage system. If multi-GB logs ever show a measured
problem, the mitigation is compressed line chunks inside SQLite.

## D14 · TS panels compiled natively on the hub with oxc (2026-09-28)

**Context:** The requirement was the best solution with no compromise. In-browser
transpilers are either heavy (esbuild-wasm is about 10 MB) or limited (sucrase only
strips types).
**Decision:** The hub embeds oxc (a Rust TS/TSX parser, transformer, and bundler) to
compile panels in about 1 ms. Errors carry file, line, and column. A shipped `.d.ts`
gives editor types. A full type check runs through `tsgo` or `tsc` when present.
**Consequences:** Nothing heavy in the browser, so phones work equally well. Imports
are limited to `trun:panel`, the vendored Plot, and relative panel files.

## D15 · User-level daemons; narrow elevation only where required (2026-09-28)

**Context:** GPU stats, process sampling, and capture all work without privileges.
Installing system services needs admin rights and widens the attack surface.
**Decision:** The hub and agents run as user daemons (systemd --user with linger,
a per-user Scheduled Task, a LaunchAgent). Host shutdown, the only privileged action,
uses polkit, `sudo -n`, or the native unprivileged path. A separate minimal privileged
helper is added only if a future feature truly needs one.
**Consequences:** `trun doctor` reports what works on each host.

## D16 · trun is the default launcher for AI-run long commands (2026-09-28)

**Context:** Visibility only works if long commands actually go through trun.
**Decision:** `trun init claude` registers the MCP server, adds a CLAUDE.md rule, and
installs a PreToolUse hook that wraps or redirects bare background commands through
`trun run`.
**Consequences:** The hook must be fast and reliable. It is implemented as a trun
subcommand that reads JSON on stdin.

## D17 · Designed from first principles (2026-09-28)

**Context:** Existing tools overlap in parts: process managers, experiment trackers,
dashboards.
**Decision:** Don't borrow designs or force-fit existing tools. Each component is
chosen or designed because it is the best answer for trun.
**Consequences:** More original work. Interop, such as importing other formats, is
fine as an optional feature but never shapes the core design.

## D18 · The name is trun (2026-09-28)

**Context:** Two rounds of alternatives were evaluated (runeye, holter, tocsin, tomte,
and others). The `trun` crate name on crates.io and the npm name are taken. PyPI is
free.
**Decision:** Keep `trun`. The binary and command are `trun`. If the tool is ever
published to crates.io, the package gets a different name (for example `trun-cli`)
and the binary stays `trun`.
**Consequences:** Installation is from GitHub releases or `cargo install --git`, which
is fine for a self-hosted single-user tool.

## D19 · Starlark for checks and parsers; plain Rust for everything built in (2026-09-28)

**Context:** D6 tentatively chose Rhai. The candidates were benchmarked on the same
workload: a regex parser over 100k synthetic log lines, and a check calling host
series functions. Windows release builds:

| Engine | Parse (ns/line) | Check (µs/eval) | Load (µs) |
|---|---|---|---|
| Plain Rust (ceiling) | 267 | 4.7 | – |
| **Starlark (starlark-rust 0.14)** | **472** | **5.3** | 771 |
| Lua 5.4 (mlua 0.12), Lua patterns | 1228 | 8.0 | 34 |
| Luau (mlua 0.12), Lua patterns | 1364 | 7.2 | 123 |
| QuickJS-NG (rquickjs 0.14) | 1358 | 7.6 | 83 |
| Rhai 1.26 | 2026 | 10.5 | 280 |

All of these build as static musl binaries for x86_64 and aarch64 (verified with
cargo-zigbuild). In the error tests, Rhai tripped on the reserved word `match`, which
is a common AI-authored name. Starlark gave the clearest, rustc-style diagnostics.

**Decision:** Starlark (starlark-rust) for user-authored checks and parsers. It
supersedes D6. Plain Rust (compiled user code, or WASM) was considered and rejected
for user files. It is only about 0.2 µs per line faster, and it would need a compile
step (5–30 s instead of about 1 ms), a toolchain or WASM runtime on every host, and a
separate sandbox. That breaks the edit→live loop (D5). Everything heavy stays native
Rust: built-in parsers, diagnosis, regex, series math, and percentiles, exposed to
Starlark as host functions.

**Consequences:** Python syntax, the language AI writes most reliably. Hermetic by
design. At an extreme 100k lines/s, parsing costs about 5% of one core. A compiled
WASM plugin tier for heavy custom parsers remains possible later, but is out of scope
for v1.

## D20 · SQLite confirmed; one database file per project (2026-09-28)

**Context:** Alternatives were reviewed against trun's workload: append-heavy writes,
range reads by run and time, one writer per database, embedded, static cross-platform
builds, and crash safety. DuckDB is weak at small streaming appends, a heavy C++
dependency, and its analytics strength is mostly redundant because of agent-side
downsampling. redb and fjall are key-value stores only, so every index would be
hand-built. Turso/Limbo is less proven. Time-series databases are servers.
**Decision:** SQLite stays (confirms D13). Storage is split into a small global
`hub.db` (hosts, samples, project registry, run index) and **one self-contained
SQLite file per project** holding its runs, events, logs, metrics, alerts, notes,
and per-run host samples. The project is resolved from `--project`, then
`.trun/config.toml`, then the git remote or root name, falling back to `_adhoc`.
**Consequences:** A project's history is portable as one file, and retention and
deletion are per project. Cross-project views go through the run index in `hub.db`.
DuckDB can still read the files as an optional, external analysis tool.

## D21 · Local clients talk to the hub over loopback HTTP with a token (2026-09-28)

**Context:** docs/02 originally planned a Unix socket or named pipe for CLI↔daemon
IPC. The browser UI, MCP, and the CLI all need the same API, and named pipes need a
separate transport on Windows.
**Decision:** One transport: HTTP on `127.0.0.1:7317`. Every `/api` call requires
the per-user token in `~/.trun/hub.token` (mode 0600), sent as a bearer header or as
an HttpOnly SameSite=Strict cookie set by the `/?t=<token>` handshake that `trun ui`
opens. Requests whose `Host` isn't loopback are rejected, which defends against DNS
rebinding.
**Consequences:** One code path for all clients. Other local users can't use the
API without the token. The CLI auto-starts the hub as a detached process (setsid on
Unix; on Windows DETACHED_PROCESS plus breakaway from the caller's job, so a harness
killing its job doesn't take the hub with it).

## D22 · Own ConPTY driver on Windows, with passthrough and a normalizer (2026-09-29)

**Context:** `--pty` on Windows via portable-pty hung every run. Its hardcoded
`PSEUDOCONSOLE_INHERIT_CURSOR` makes ConPTY ask the absent terminal for the cursor
position. It also can't enable passthrough. Without passthrough, ConPTY re-renders
its screen and emits cursor-positioning diffs instead of the program's `\r` and `\n`,
so line parsing saw glued-together redraws.
**Decision:** On Windows, trun drives ConPTY directly (`trun-supervise/src/conpty.rs`).
It requests `PSEUDOCONSOLE_PASSTHROUGH_MODE` (raw program output where the OS
supports it) and never inherit-cursor. It passes an explicit application path and
invalid std handles (so the child can't grab the hub's log). It answers terminal
queries itself (DSR, DA1). A normalizer maps rendered-mode positioning back to
`\r`/`\n` and drops screen clears. Unix keeps portable-pty, because openpty is
transparent. `TRUN_PTY_TRACE=<file>` dumps raw pseudo-console bytes for debugging.
**Consequences:** tqdm under `--pty` parses the same on Windows as on Linux. A full
terminal emulator was avoided. If a program relies on complex full-screen redraws,
its log is approximate, but its structure (steps, progress) is still extracted.

## D23 · On Windows the hub runs from a copy; outdated hubs restart (2026-09-29)

**Context:** Windows locks a running .exe. A background hub started from `trun.exe`
blocked rebuilding or upgrading trun, and a stale hub kept serving old code.
**Decision:** The auto-started hub runs from `$TRUN_HOME/bin/trun-hub-<size>-<mtime>.exe`,
and old copies are cleaned up. `/api/health` reports a `build` id (size and mtime,
which a copy preserves). When the CLI finds a different build, it restarts the hub
if no runs are active. Otherwise it prints a note.
**Consequences:** Upgrading trun just works. Unix uses the binary in place, because
a running file can be replaced there.

## D24 · No handle leaks into background processes (2026-09-29)

**Context:** `CreateProcess` passes every inheritable handle to the child. The
auto-started hub inherited the CLI's stdout pipe and held it open forever, so
`$(trun run …)`, `trun … | cat`, or an agent harness capturing output hung waiting
for EOF.
**Decision:** Before spawning the hub, the CLI marks its std handles
non-inheritable. The hub does the same for itself, so runs don't inherit its log
file. The client also checks that the hub on the port accepts *its* token and
explains when a hub from another `TRUN_HOME` holds the port. A `hub stop` gives open
SSE connections 2 s, then exits.
**Consequences:** trun is safe to call from any capturing context, which is the
normal case for AI agents.

## D25 · Float fields survive serde_json `arbitrary_precision` (2026-09-29)

**Context:** starlark enables serde_json's `arbitrary_precision`, and Cargo unifies
features across the build. Under it, floats in buffered serde paths (internally
tagged enums, `flatten`, untagged) arrive as a private map. `Progress` events,
progress directives, and `FleetEvent`-wrapped step state failed to decode with
"invalid type: map, expected f64". The hub couldn't read back summaries it had
stored.
**Decision:** `Num` has an explicit visitor that also accepts the map form. Every
`f64`/`Option<f64>` field in trun-proto uses `flex::f64`/`flex::opt_f64`. A
regression test lives in trun-checks, so it always builds with the feature on.
**Consequences:** New float fields in proto types must use `flex` (or `Num`).
Integers are unaffected.

## D26 · Check API: flat run methods, seconds everywhere, `None` for no data (2026-09-29)

**Context:** The first sketch used nested objects (`run.host.gpu.util`) and mixed
units. AI-written checks do best with a small, uniform surface.
**Decision:** Flat methods on `run` (`silence()`, `proc_cpu()`, `metric()`,
`steps()`, …) returning series. All durations are seconds (`mins(15)` = 900).
Aggregations over no data return `None`, and `is_nan(None)` is `False`. The
built-in defaults are written in Starlark with the same API and embedded in the
binary. `RunData` is built from the stored events, so `trun check test` replays
exactly what the live check saw.
**Consequences:** Checks must guard `!= None` before comparing aggregates (the docs'
examples do). Sampled series (process, host, GPU) are live-only until samples are
recorded (M4).
