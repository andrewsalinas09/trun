<script lang="ts">
  import { AnsiUp } from "ansi_up";
  import { api, subscribe, Unauthorized } from "../lib/api";
  import { ago, clock, command, duration, elapsed } from "../lib/format";
  import { now } from "../lib/now.svelte";
  import { TERMINAL, type LogLine, type RunEvent, type RunSummary } from "../lib/types";
  import AuthNotice from "./AuthNotice.svelte";
  import LogView, { type ViewLine } from "./LogView.svelte";
  import StateBadge from "./StateBadge.svelte";

  let { id }: { id: string } = $props();

  const INITIAL_TAIL = 2000;
  const MAX_LINES = 20000;

  let run = $state<RunSummary | null>(null);
  let lines = $state<ViewLine[]>([]);
  let hasEarlier = $state(false);
  let error = $state<string | null>(null);
  let unauthorized = $state(false);
  let cancelling = $state(false);

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

  function onEvent(ev: RunEvent) {
    if (!run) return;
    if (ev.kind === "output") {
      append(toView(ev));
      run.last_seq = ev.seq;
      run.last_output_at = ev.ts;
    } else if (ev.kind === "lifecycle") {
      run.lifecycle = ev.state;
      if (ev.pid != null) run.pid = ev.pid;
      if (ev.state === "running" && run.started_at == null) run.started_at = ev.ts;
      if (TERMINAL.has(ev.state)) {
        run.ended_at = ev.ts;
        run.exit_code = ev.exit_code ?? null;
        run.signal = ev.signal ?? null;
      }
    } else if (ev.kind === "diagnosis") {
      const { cause, summary, early, evidence } = ev;
      run.diagnosis = { cause, summary, early, evidence };
    }
  }

  $effect(() => {
    let close: (() => void) | null = null;
    let cancelled = false;
    (async () => {
      try {
        const r = await api.run(id);
        const page = await api.logs(r.id, { tail: INITIAL_TAIL });
        if (cancelled) return;
        run = r;
        for (const l of page.lines as LogLine[]) append(toView(l));
        hasEarlier = page.lines.length >= INITIAL_TAIL;
        const since = page.lines.length ? page.lines[page.lines.length - 1].seq : 0;
        close = subscribe(`/runs/${r.id}/events?since_seq=${since}`, {
          event: (d) => onEvent(d as RunEvent),
          end: (d) => {
            run = d as RunSummary;
            close?.();
          },
        });
      } catch (e) {
        if (e instanceof Unauthorized) unauthorized = true;
        else error = String((e as Error).message ?? e);
      }
    })();
    return () => {
      cancelled = true;
      close?.();
    };
  });

  async function loadEarlier() {
    if (!run || lines.length === 0) return;
    const page = await api.logs(run.id, { tail: INITIAL_TAIL, before_seq: lines[0].seq });
    const older = page.lines.map(toView);
    const shift = older.length;
    lines.unshift(...older);
    for (const s of ["stdout", "stderr"]) {
      const i = provisional[s];
      if (i != null) provisional[s] = i + shift;
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
</script>

{#if unauthorized}
  <AuthNotice />
{:else if error && !run}
  <p class="error">{error}</p>
{:else if run}
  <section class="head">
    <div class="title">
      <h1>{run.name}</h1>
      <StateBadge {run} />
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

  {#if run.diagnosis}
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

  {#if error}<p class="error">{error}</p>{/if}

  <LogView {lines} canLoadEarlier={hasEarlier} onLoadEarlier={loadEarlier} />
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
    margin-bottom: 12px;
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
  .error {
    color: var(--bad);
  }
</style>
