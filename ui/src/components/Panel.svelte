<script lang="ts">
  // One panel from a .trun/panels/*.toml file. Invalid files render as an error
  // card naming the file, and the rest of the dashboard keeps working.
  import { fmtNum, reduce, type Pt } from "../lib/metrics.svelte";
  import { now } from "../lib/now.svelte";
  import { yNames, type Loaded, type PanelSpec } from "../lib/types";
  import MetricChart from "./MetricChart.svelte";

  let {
    panel,
    series,
    startedAt,
    height = null,
  }: {
    panel: Loaded<PanelSpec>;
    series: Record<string, Pt[]>;
    startedAt: number | null;
    height?: number | null;
  } = $props();

  const spec = $derived(panel.spec);
  const names = $derived(spec ? yNames(spec) : []);
  const title = $derived(spec?.title ?? panel.name);
  const missing = $derived(names.filter((n) => !(n in series)));

  function statColor(v: number | null): string | null {
    if (!spec || v == null) return null;
    for (const t of spec.thresholds) {
      if ((t.below != null && v < t.below) || (t.above != null && v > t.above)) {
        return ({ warn: "var(--warn)", bad: "var(--bad)", ok: "var(--ok)" } as Record<string, string>)[t.color] ?? t.color;
      }
    }
    return null;
  }

  function stats(pts: Pt[]) {
    const f = pts.filter((p) => Number.isFinite(p.v)).map((p) => p.v);
    return {
      last: pts.length ? pts[pts.length - 1].v : null,
      min: f.length ? Math.min(...f) : null,
      max: f.length ? Math.max(...f) : null,
      count: pts.length,
      bad: pts.length - f.length,
    };
  }
</script>

<section class="panel" class:error={!!panel.error}>
  <header>
    <span class="title">{title}</span>
    <span class="src muted" title={panel.file}>{panel.scope === "global" ? "global · " : ""}{panel.name}</span>
  </header>

  {#if panel.error || !spec}
    <div class="err">
      <div class="mono file">{panel.file}</div>
      <pre class="mono">{panel.error}</pre>
    </div>
  {:else if names.every((n) => !(n in series))}
    <div class="empty muted">
      Waiting for metric{names.length > 1 ? "s" : ""}
      <span class="mono">{names.join(", ")}</span>
    </div>
  {:else if spec.kind === "stat"}
    {@const v = reduce(series[names[0]] ?? [], spec.reduce, now.value)}
    <div class="stat" style:color={statColor(v)}>
      {fmtNum(v)}{#if spec.unit}<span class="unit">{spec.unit}</span>{/if}
    </div>
    <div class="muted small">{names[0]} · {spec.reduce ?? "last"}</div>
  {:else if spec.kind === "table"}
    <table class="mono">
      <thead><tr><th>metric</th><th>last</th><th>min</th><th>max</th><th>points</th></tr></thead>
      <tbody>
        {#each names as n (n)}
          {@const s = stats(series[n] ?? [])}
          <tr>
            <td>{n}</td><td>{fmtNum(s.last)}</td><td>{fmtNum(s.min)}</td><td>{fmtNum(s.max)}</td>
            <td>{s.count}{#if s.bad}<span class="badnum"> ({s.bad} non-finite)</span>{/if}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else}
    <MetricChart
      {names}
      {series}
      kind={spec.kind}
      x={spec.x}
      smooth={spec.smooth ?? null}
      logY={spec.scale?.y === "log"}
      height={height ?? spec.height ?? 200}
      {startedAt}
    />
    {#if missing.length}
      <div class="muted small">no data yet: {missing.join(", ")}</div>
    {/if}
  {/if}
</section>

<style>
  .panel {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 10px 12px;
    min-width: 0;
  }
  .panel.error {
    border-color: color-mix(in srgb, var(--bad) 45%, var(--border));
  }
  header {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 6px;
  }
  .title {
    font-weight: 600;
  }
  .src {
    font-size: 11px;
  }
  .err pre {
    margin: 6px 0 0;
    white-space: pre-wrap;
    color: var(--bad);
    font-size: 12px;
  }
  .file {
    font-size: 11px;
    color: var(--muted);
    word-break: break-all;
  }
  .empty {
    padding: 24px 0;
    text-align: center;
  }
  .stat {
    font-size: 34px;
    font-weight: 600;
    line-height: 1.2;
    padding: 8px 0 2px;
  }
  .unit {
    font-size: 14px;
    margin-left: 6px;
    color: var(--muted);
  }
  .small {
    font-size: 12px;
  }
  table {
    width: 100%;
    border-collapse: collapse;
  }
  th,
  td {
    text-align: left;
    padding: 3px 6px;
    border-bottom: 1px solid var(--border);
  }
  th {
    color: var(--muted);
    font-weight: 500;
  }
  .badnum {
    color: var(--bad);
  }
</style>
