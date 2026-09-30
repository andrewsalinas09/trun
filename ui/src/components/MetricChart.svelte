<script lang="ts">
  // One chart of one or more metric series, rendered with Observable Plot.
  // Non-finite values (NaN, inf) are drawn as red rules: they are usually the most
  // important thing on the chart.
  import * as Plot from "@observablehq/plot";
  import type { Pt } from "../lib/metrics.svelte";

  let {
    names,
    series,
    kind = "line",
    x = "step",
    smooth = null,
    logY = false,
    height = 180,
    startedAt = null,
  }: {
    names: string[];
    series: Record<string, Pt[]>;
    kind?: "line" | "area" | "scatter" | "bar";
    x?: "step" | "time" | "elapsed";
    smooth?: number | null;
    logY?: boolean;
    height?: number;
    startedAt?: number | null;
  } = $props();

  let el: HTMLDivElement;
  let width = $state(400);

  interface Row {
    name: string;
    x: number | Date;
    v: number;
  }

  function xOf(p: Pt, i: number): number | Date {
    if (x === "time") return new Date(p.t);
    if (x === "elapsed") return (p.t - (startedAt ?? p.t)) / 1000;
    return p.s ?? i; // step, falling back to point index
  }

  const data = $derived.by(() => {
    const rows: Row[] = [];
    const smoothed: Row[] = [];
    const bad: Row[] = [];
    for (const name of names) {
      const pts = series[name] ?? [];
      let ema: number | null = null;
      pts.forEach((p, i) => {
        const r = { name, x: xOf(p, i), v: p.v };
        if (!Number.isFinite(p.v)) {
          bad.push(r);
          return;
        }
        if (logY && p.v <= 0) return;
        rows.push(r);
        if (smooth) {
          ema = ema == null ? p.v : smooth * ema + (1 - smooth) * p.v;
          smoothed.push({ ...r, v: ema });
        }
      });
    }
    return { rows, smoothed, bad };
  });

  const xLabel = $derived(x === "time" ? null : x === "elapsed" ? "seconds" : "step");

  $effect(() => {
    const { rows, smoothed, bad } = data;
    const multi = names.length > 1;
    const stroke = multi ? "name" : "var(--accent)";
    const marks: Plot.Markish[] = [];
    if (kind === "bar") {
      // Latest value per metric.
      const last = names
        .map((name) => {
          const pts = (series[name] ?? []).filter((p) => Number.isFinite(p.v));
          return pts.length ? { name, v: pts[pts.length - 1].v } : null;
        })
        .filter((r): r is { name: string; v: number } => r != null);
      marks.push(Plot.barY(last, { x: "name", y: "v", fill: multi ? "name" : "var(--accent)", tip: true }));
    } else {
      if (kind === "area") {
        // An area's default baseline is y=0, which a log scale maps to -Infinity; d3's
        // log ticks then never terminate and freeze the page. Fill down to the smallest
        // plotted value instead.
        const floor = logY && rows.length ? Math.min(...rows.map((r) => r.v)) : 0;
        marks.push(Plot.areaY(rows, { x: "x", y1: floor, y2: "v", fill: stroke, fillOpacity: 0.15 }));
      }
      if (kind === "scatter") {
        marks.push(Plot.dot(rows, { x: "x", y: "v", stroke, r: 2 }));
      } else {
        marks.push(Plot.lineY(rows, { x: "x", y: "v", stroke, strokeOpacity: smoothed.length ? 0.25 : 1 }));
        if (smoothed.length) marks.push(Plot.lineY(smoothed, { x: "x", y: "v", stroke }));
      }
      if (bad.length) marks.push(Plot.ruleX(bad, { x: "x", stroke: "var(--bad)", strokeWidth: 1.5, strokeDasharray: "3,2" }));
      marks.push(Plot.tip(smoothed.length ? smoothed : rows, Plot.pointerX({ x: "x", y: "v", stroke: multi ? "name" : undefined })));
    }
    const chart = Plot.plot({
      width,
      height,
      marginLeft: 48,
      style: { background: "transparent", color: "var(--muted)", fontSize: "11px" },
      x: { label: kind === "bar" ? null : xLabel, type: kind === "bar" ? "band" : x === "time" ? "time" : "linear" },
      y: { grid: true, label: null, type: logY ? "log" : "linear" },
      color: multi ? { legend: true } : undefined,
      marks,
    });
    el.replaceChildren(chart);
    return () => chart.remove();
  });
</script>

<div class="chart" bind:this={el} bind:clientWidth={width}></div>

<style>
  .chart {
    width: 100%;
    min-height: 60px;
  }
  .chart :global(svg) {
    overflow: visible;
  }
</style>
