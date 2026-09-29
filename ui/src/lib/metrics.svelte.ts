// Metric series for one run: history from the API, then live points from the run's
// event stream. Live points are buffered and flushed at most every FLUSH_MS so a
// program emitting hundreds of metrics per second doesn't re-render charts per point.

import { api } from "./api";
import { num, type MetricPoint } from "./types";

export interface Pt {
  s: number | null;
  t: number;
  v: number;
}

const FLUSH_MS = 500;

function toPt(p: MetricPoint): Pt {
  return { s: p.s ?? null, t: p.t, v: num(p.v) };
}

export class MetricStore {
  series = $state<Record<string, Pt[]>>({});
  downsampled = $state<Record<string, boolean>>({});
  private pending: Record<string, Pt[]> = {};
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(private runId: string) {}

  /** Load history for every metric the run has reported so far. */
  async load(): Promise<void> {
    const all = await api.metrics(this.runId);
    const next: Record<string, Pt[]> = {};
    const ds: Record<string, boolean> = {};
    for (const s of all) {
      const pts = s.points.map(toPt);
      // Keep live points that arrived while history was loading.
      const lastT = pts.length ? pts[pts.length - 1].t : -Infinity;
      const live = (this.series[s.name] ?? []).filter((p) => p.t > lastT);
      next[s.name] = pts.concat(live);
      ds[s.name] = s.downsampled;
    }
    for (const [name, pts] of Object.entries(this.series)) {
      if (!(name in next)) next[name] = pts;
    }
    this.series = next;
    this.downsampled = ds;
  }

  push(values: Record<string, number>, step: number | null, t: number): void {
    for (const [name, v] of Object.entries(values)) {
      (this.pending[name] ??= []).push({ s: step, t, v });
    }
    this.timer ??= setTimeout(() => this.flush(), FLUSH_MS);
  }

  private flush(): void {
    this.timer = null;
    const next = { ...this.series };
    for (const [name, pts] of Object.entries(this.pending)) {
      next[name] = (next[name] ?? []).concat(pts);
    }
    this.pending = {};
    this.series = next;
  }

  dispose(): void {
    if (this.timer) clearTimeout(this.timer);
  }
}

/** Reduce a series for stat tiles: last | min | max | mean | avg:<duration>. */
export function reduce(pts: Pt[], how: string | null | undefined, now: number): number | null {
  const finite = pts.filter((p) => Number.isFinite(p.v));
  if (!pts.length) return null;
  const mode = how ?? "last";
  if (mode === "last") return pts[pts.length - 1].v;
  if (!finite.length) return NaN;
  if (mode === "min") return Math.min(...finite.map((p) => p.v));
  if (mode === "max") return Math.max(...finite.map((p) => p.v));
  let window = finite;
  if (mode.startsWith("avg:")) {
    const ms = parseDuration(mode.slice(4));
    // The window ends at the latest point, so a finished run still shows its
    // final average instead of an empty window.
    const end = Math.min(now, finite[finite.length - 1].t);
    if (ms != null) window = finite.filter((p) => p.t >= end - ms);
  }
  if (!window.length) return null;
  return window.reduce((a, p) => a + p.v, 0) / window.length;
}

export function parseDuration(s: string): number | null {
  const m = s.trim().match(/^(\d+(?:\.\d+)?)(ms|s|m|h|d)?$/);
  if (!m) return null;
  const v = parseFloat(m[1]);
  const mult = { ms: 1, s: 1000, m: 60_000, h: 3_600_000, d: 86_400_000 }[m[2] ?? "s"]!;
  return v * mult;
}

/** Compact number formatting, matching the CLI. */
export function fmtNum(v: number | null): string {
  if (v == null) return "–";
  if (Number.isNaN(v)) return "NaN";
  if (!Number.isFinite(v)) return v > 0 ? "inf" : "-inf";
  const a = Math.abs(v);
  if (Number.isInteger(v) && a < 1e12) return String(v);
  if (a !== 0 && (a < 1e-3 || a >= 1e6)) return v.toExponential(3);
  return String(Number(v.toFixed(4)));
}
