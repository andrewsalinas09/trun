<script lang="ts">
  // Small multiples: one chart per metric the run reports (minus those already on
  // a custom panel). Works with zero configuration.
  import { fmtNum, type Pt } from "../lib/metrics.svelte";
  import MetricChart from "./MetricChart.svelte";

  let {
    series,
    exclude = [],
    startedAt,
  }: { series: Record<string, Pt[]>; exclude?: string[]; startedAt: number | null } = $props();

  const names = $derived(
    Object.keys(series)
      .filter((n) => !exclude.includes(n))
      .sort(),
  );

  function last(n: string): number | null {
    const pts = series[n];
    return pts?.length ? pts[pts.length - 1].v : null;
  }

  function nonFinite(n: string): number {
    return (series[n] ?? []).filter((p) => !Number.isFinite(p.v)).length;
  }
</script>

{#if names.length}
  <div class="grid">
    {#each names as n (n)}
      {@const bad = nonFinite(n)}
      <section class="card" class:alarm={bad > 0}>
        <header>
          <span class="mono name">{n}</span>
          <span class="mono last">{fmtNum(last(n))}</span>
        </header>
        {#if bad}<div class="bad">{bad} non-finite value{bad > 1 ? "s" : ""}</div>{/if}
        <MetricChart names={[n]} {series} height={120} {startedAt} />
      </section>
    {/each}
  </div>
{/if}

<style>
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 10px;
  }
  .card {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 8px 10px 4px;
    min-width: 0;
  }
  .card.alarm {
    border-color: color-mix(in srgb, var(--bad) 45%, var(--border));
  }
  header {
    display: flex;
    justify-content: space-between;
    gap: 8px;
  }
  .name {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .last {
    color: var(--muted);
  }
  .bad {
    color: var(--bad);
    font-size: 12px;
  }
</style>
