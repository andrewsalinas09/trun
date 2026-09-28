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
