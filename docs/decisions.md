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
