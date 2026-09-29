<script lang="ts">
  import { AnsiUp } from "ansi_up";
  import { api, subscribe, Unauthorized } from "../lib/api";
  import { ago, clock, command, duration, elapsed } from "../lib/format";
  import { MetricStore, type Pt } from "../lib/metrics.svelte";
  import { now } from "../lib/now.svelte";
  import {
    num,
    TERMINAL,
    LEVEL_RANK,
    healthOf,
    yNames,
    type LogLine,
    type PanelSet,
    type RunEvent,
    type RunSummary,
    type StepState,
  } from "../lib/types";
  import AuthNotice from "./AuthNotice.svelte";
  import AutoMetrics from "./AutoMetrics.svelte";
  import LogView, { type ViewLine } from "./LogView.svelte";
  import Messages, { type Message } from "./Messages.svelte";
  import Panel from "./Panel.svelte";
  import StateBadge from "./StateBadge.svelte";
  import Alerts from "./Alerts.svelte";
  import Steps from "./Steps.svelte";

  let { id }: { id: string } = $props();

  const INITIAL_TAIL = 2000;
  const MAX_LINES = 20000;

  let run = $state<RunSummary | null>(null);
  let lines = $state<ViewLine[]>([]);
  let messages = $state<Message[]>([]);
  let panelSet = $state<PanelSet | null>(null);
  let hasEarlier = $state(false);
  let error = $state<string | null>(null);
  let unauthorized = $state(false);
  let cancelling = $state(false);
  let metrics: MetricStore | null = null;
  let series = $state.raw<Record<string, Pt[]>>({});

  const ansi = new AnsiUp();
  ansi.use_classes = false;
  // Index into `lines` of the current provisional (\r) line per stream.
  const provisional: Record<string, number | null> = { stdout: null, stderr: null };

  function toView(l: { seq: number; stream: "stdout" | "stderr"; text: string; cr: boolean }): ViewLine {
    return { seq: l.seq, stream: l.stream, text: l.text, cr: l.cr, html: ansi.ansi_to_html(l.text) };
  }

  function append(l: ViewLine) {
    const p = provisional[l.stream];
    if (p != null && p < lines.length && lines[p].cr) {
      lines[p] = l; // a provisional line is replaced by the next line on its stream
      provisional[l.stream] = l.cr ? p : null;
      return;
    }
    lines.push(l);
    provisional[l.stream] = l.cr ? lines.length - 1 : null;
    if (lines.length > MAX_LINES) {
      const drop = lines.length - MAX_LINES;
      lines.splice(0, drop);
      for (const s of ["stdout", "stderr"]) {
        const i = provisional[s];
        provisional[s] = i == null ? null : i - drop >= 0 ? i - drop : null;
      }
      hasEarlier = true;
    }
  }

  function step(id: string): StepState | undefined {
    return run?.steps.find((s) => s.id === id);
  }

  function addMessage(ev: RunEvent) {
    if (ev.kind === "note") messages.push({ seq: ev.seq, ts: ev.ts, level: "note", text: ev.text });
    else if (ev.kind === "log") messages.push({ seq: ev.seq, ts: ev.ts, level: ev.level, text: ev.text });
    else if (ev.kind === "check_error")
      messages.push({ seq: ev.seq, ts: ev.ts, level: "error", text: `check ${ev.check}: ${ev.error.split("\n").slice(0, 3).join(" · ")}` });
    else if (ev.kind === "alert") {
      const level = ev.level === "fail" ? "error" : ev.level === "info" ? "info" : "warn";
      messages.push({ seq: ev.seq, ts: ev.ts, level, text: `${ev.state} [${ev.level}] ${ev.message}` });
    }
  }

  function onAlert(ev: Extract<RunEvent, { kind: "alert" }>) {
    if (!run) return;
    const rest = run.alerts.filter((a) => a.key !== ev.key);
    if (ev.state === "cleared") {
      run.alerts = rest;
    } else {
      const prev = run.alerts.find((a) => a.key === ev.key);
      run.alerts = [
        ...rest,
        {
          key: ev.key,
          check: ev.check,
          level: ev.level,
          message: ev.message,
          opened_at: prev?.opened_at ?? ev.ts,
          last_at: ev.ts,
          count: (prev?.count ?? 0) + 1,
        },
      ].sort((a, b) => LEVEL_RANK[b.level] - LEVEL_RANK[a.level]);
    }
    run.health = healthOf(run.alerts);
  }

  function onEvent(ev: RunEvent) {
    if (!run) return;
    run.last_seq = ev.seq;
    switch (ev.kind) {
      case "output":
        append(toView(ev));
        run.last_output_at = ev.ts;
        break;
      case "lifecycle":
        run.lifecycle = ev.state;
        if (ev.pid != null) run.pid = ev.pid;
        if (ev.state === "running" && run.started_at == null) run.started_at = ev.ts;
        if (TERMINAL.has(ev.state)) {
          run.ended_at = ev.ts;
          run.exit_code = ev.exit_code ?? null;
          run.signal = ev.signal ?? null;
        }
        break;
      case "diagnosis": {
        const { cause, summary, early, evidence } = ev;
        run.diagnosis = { cause, summary, early, evidence };
        break;
      }
      case "step_begin": {
        const s = step(ev.id);
        if (s) {
          s.state = "running";
          s.name = ev.name;
          s.ended_at = null;
        } else {
          run.steps.push({
            id: ev.id,
            parent: ev.parent ?? null,
            name: ev.name,
            state: "running",
            current: null,
            total: null,
            unit: null,
            rate: null,
            eta_ms: null,
            started_at: ev.ts,
            ended_at: null,
            progressed_at: null,
          });
        }
        break;
      }
      case "step_end": {
        const s = step(ev.id);
        if (s) {
          s.state = ev.status;
          s.ended_at = ev.ts;
          s.eta_ms = null;
        }
        break;
      }
      case "progress": {
        const s = step(ev.id);
        if (s) {
          s.current = ev.current;
          s.total = ev.total ?? s.total;
          s.unit = ev.unit ?? s.unit;
          s.rate = ev.rate ?? null;
          s.eta_ms = ev.eta_ms ?? null;
          s.progressed_at = ev.ts;
        }
        break;
      }
      case "metric": {
        const vals: Record<string, number> = {};
        for (const [k, v] of Object.entries(ev.values)) vals[k] = num(v);
        metrics?.push(vals, ev.step ?? null, ev.ts);
        break;
      }
      case "log":
      case "note":
      case "check_error":
        addMessage(ev);
        break;
      case "alert":
        onAlert(ev);
        addMessage(ev);
        break;
    }
  }

  $effect(() => {
    let close: (() => void) | null = null;
    let closePanels: (() => void) | null = null;
    let cancelled = false;
    let store: MetricStore | null = null;
    (async () => {
      try {
        const r = await api.run(id);
        const [page, msgs] = await Promise.all([api.logs(r.id, { tail: INITIAL_TAIL }), api.messages(r.id)]);
        if (cancelled) return;
        run = r;
        for (const m of msgs) addMessage(m);
        for (const l of page.lines as LogLine[]) append(toView(l));
        hasEarlier = page.lines.length >= INITIAL_TAIL;
        store = new MetricStore(r.id, (s) => (series = s));
        metrics = store;
        const since = Math.max(page.lines.length ? page.lines[page.lines.length - 1].seq : 0, msgs.length ? msgs[msgs.length - 1].seq : 0);
        close = subscribe(`/runs/${r.id}/events?since_seq=${since}`, {
          event: (d) => onEvent(d as RunEvent),
          end: (d) => {
            const final = d as RunSummary;
            run = final;
            close?.();
            store?.load();
          },
        });
        closePanels = subscribe(`/runs/${r.id}/panels/stream`, {
          panels: (d) => (panelSet = d as PanelSet),
        });
        await store.load();
      } catch (e) {
        if (e instanceof Unauthorized) unauthorized = true;
        else error = String((e as Error).message ?? e);
      }
    })();
    return () => {
      cancelled = true;
      close?.();
      closePanels?.();
      store?.dispose();
    };
  });

  async function loadEarlier() {
    if (!run || lines.length === 0) return;
    const page = await api.logs(run.id, { tail: INITIAL_TAIL, before_seq: lines[0].seq });
    const older = page.lines.map(toView);
    lines.unshift(...older);
    for (const s of ["stdout", "stderr"]) {
      const i = provisional[s];
      if (i != null) provisional[s] = i + older.length;
    }
    hasEarlier = page.lines.length >= INITIAL_TAIL;
  }

  async function cancel(force: boolean) {
    if (!run) return;
    cancelling = true;
    try {
      await api.cancel(run.id, force);
    } catch (e) {
      error = String((e as Error).message ?? e);
    }
  }

  const active = $derived(run != null && !TERMINAL.has(run.lifecycle));
  const el = $derived(run ? elapsed(run, now.value) : null);
  const dashboard = $derived(panelSet?.dashboards.find((d) => d.name === panelSet?.dashboard && d.spec) ?? null);
  const dashboardHasAlerts = $derived(
    !!dashboard?.spec?.row.some((r) => r.panels.some((c) => c.panel === "builtin:alerts")),
  );
  const panelsByName = $derived(Object.fromEntries((panelSet?.panels ?? []).map((p) => [p.name, p])));
  const plottedNames = $derived(
    (panelSet?.panels ?? []).flatMap((p) => (p.spec ? yNames(p.spec) : [])),
  );
</script>

{#snippet stepsSection()}
  {#if run && run.steps.length}
    <h2>Steps</h2>
    <Steps steps={run.steps} />
  {/if}
{/snippet}

{#snippet metricsSection(exclude: string[])}
  {#if Object.keys(series).some((n) => !exclude.includes(n))}
    <h2>Metrics</h2>
    <AutoMetrics {series} {exclude} startedAt={run?.started_at ?? null} />
  {/if}
{/snippet}

{#snippet logSection()}
  <h2>Output</h2>
  <LogView {lines} canLoadEarlier={hasEarlier} onLoadEarlier={loadEarlier} />
{/snippet}

{#snippet diagnosisSection()}
  {#if run?.diagnosis}
    <section class="diagnosis">
      <div class="cause">
        <strong>{run.diagnosis.cause}</strong>
        {#if run.diagnosis.early}<span class="early">died early</span>{/if}
      </div>
      <div>{run.diagnosis.summary}</div>
      {#if run.diagnosis.evidence.length}
        <pre class="evidence mono">{#each run.diagnosis.evidence as e (e.seq)}<span class="faint">[{e.seq}]</span> {e.text}
{/each}</pre>
      {/if}
    </section>
  {/if}
{/snippet}

{#snippet builtin(name: string)}
  {#if name === "builtin:steps"}{@render stepsSection()}
  {:else if name === "builtin:metrics"}{@render metricsSection(plottedNames)}
  {:else if name === "builtin:log"}{@render logSection()}
  {:else if name === "builtin:diagnosis"}{@render diagnosisSection()}
  {:else if name === "builtin:alerts"}<Alerts alerts={run?.alerts ?? []} />
  {/if}
{/snippet}

{#if unauthorized}
  <AuthNotice />
{:else if error && !run}
  <p class="error">{error}</p>
{:else if run}
  <section class="head">
    <div class="title">
      <h1>{run.name}</h1>
      <StateBadge {run} />
      {#if run.pty}<span class="tag">pty</span>{/if}
      <div class="spacer"></div>
      {#if active}
        {#if cancelling}
          <button class="danger" onclick={() => cancel(true)}>Force kill</button>
        {:else}
          <button class="danger" onclick={() => cancel(false)}>Cancel</button>
        {/if}
      {/if}
    </div>
    <dl>
      <div><dt>Duration</dt><dd class="mono">{el == null ? "–" : duration(el)}</dd></div>
      <div>
        <dt>Last output</dt>
        <dd>
          {#if run.last_output_at}
            {active ? ago(run.last_output_at, now.value) : clock(run.last_output_at)}
          {:else}<span class="faint">none</span>{/if}
        </dd>
      </div>
      <div><dt>Project</dt><dd>{run.project}</dd></div>
      <div><dt>Host</dt><dd>{run.host}{run.pid ? ` · pid ${run.pid}` : ""}</dd></div>
      <div><dt>Started</dt><dd>{clock(run.created_at)}</dd></div>
      <div class="wide"><dt>Command</dt><dd class="mono">{command(run.cmd)}</dd></div>
      <div class="wide"><dt>Directory</dt><dd class="mono">{run.cwd}</dd></div>
      <div class="wide"><dt>Run id</dt><dd class="mono">{run.id}</dd></div>
    </dl>
  </section>

  {#if error}<p class="error">{error}</p>{/if}
  {#if !dashboardHasAlerts}<Alerts alerts={run.alerts ?? []} />{/if}

  {#if dashboard?.spec}
    {#if dashboard.error}<p class="error">dashboard {dashboard.name}: {dashboard.error}</p>{/if}
    {#if dashboard.spec.title}<h2 class="dash-title">{dashboard.spec.title}</h2>{/if}
    {#each dashboard.spec.row as row, ri (ri)}
      <div class="row" style:grid-template-columns="repeat({dashboard.spec.columns ?? 12}, minmax(0, 1fr))">
        {#each row.panels as cell, ci (ci)}
          <div class="cell" style:grid-column="span {cell.span ?? dashboard.spec.columns ?? 12}">
            {#if cell.panel.startsWith("builtin:")}
              {@render builtin(cell.panel)}
            {:else if panelsByName[cell.panel]}
              <Panel panel={panelsByName[cell.panel]} {series} startedAt={run.started_at} height={cell.height ?? null} />
            {/if}
          </div>
        {/each}
      </div>
    {/each}
    <Messages {messages} />
  {:else}
    {@render diagnosisSection()}
    <Messages {messages} />
    {@render stepsSection()}
    {#if panelSet?.panels.length}
      <h2>Panels</h2>
      <div class="panels">
        {#each panelSet.panels as p (p.name)}
          <Panel panel={p} {series} startedAt={run.started_at} />
        {/each}
      </div>
    {/if}
    {#each (panelSet?.dashboards ?? []).filter((d) => d.error) as d (d.name)}
      <p class="error">dashboard {d.name} ({d.file}): {d.error}</p>
    {/each}
    {@render metricsSection(plottedNames)}
    {@render logSection()}
  {/if}
{:else}
  <p class="muted">Loading…</p>
{/if}

<style>
  .head {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 14px 16px;
    margin-bottom: 12px;
  }
  .title {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  h1 {
    font-size: 18px;
    margin: 0;
  }
  h2 {
    font-size: 13px;
    font-weight: 600;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    margin: 18px 0 8px;
  }
  .tag {
    font-size: 11px;
    padding: 0 6px;
    border: 1px solid var(--border);
    border-radius: 999px;
    color: var(--muted);
  }
  .spacer {
    flex: 1;
  }
  dl {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
    gap: 10px 20px;
    margin: 12px 0 0;
  }
  dl .wide {
    grid-column: 1 / -1;
  }
  dt {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted);
  }
  dd {
    margin: 2px 0 0;
    word-break: break-all;
  }
  .diagnosis {
    background: var(--bad-bg);
    border: 1px solid color-mix(in srgb, var(--bad) 35%, transparent);
    border-radius: 10px;
    padding: 12px 16px;
    margin: 12px 0;
  }
  .cause {
    display: flex;
    gap: 10px;
    align-items: center;
    color: var(--bad);
    margin-bottom: 4px;
  }
  .early {
    font-size: 11px;
    padding: 0 6px;
    border: 1px solid currentColor;
    border-radius: 999px;
  }
  .evidence {
    margin: 8px 0 0;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .panels {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(360px, 1fr));
    gap: 10px;
  }
  .row {
    display: grid;
    gap: 10px;
    margin-bottom: 10px;
  }
  .cell {
    min-width: 0;
  }
  .cell > :global(h2:first-child) {
    margin-top: 0;
  }
  .dash-title {
    margin-top: 6px;
  }
  .error {
    color: var(--bad);
  }
</style>
