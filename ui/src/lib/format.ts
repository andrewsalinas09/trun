import type { RunSummary } from "./types";

export function duration(ms: number): string {
  ms = Math.max(0, ms);
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${(ms / 1000).toFixed(1)}s`;
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  return h > 0 ? `${h}h${String(m).padStart(2, "0")}m` : `${m}m${String(r).padStart(2, "0")}s`;
}

export function ago(ts: number, now: number): string {
  const d = now - ts;
  if (d < 5000) return "just now";
  return `${duration(d)} ago`;
}

export function elapsed(r: RunSummary, now: number): number | null {
  if (r.started_at == null) return null;
  return (r.ended_at ?? now) - r.started_at;
}

export function stateLabel(r: RunSummary): string {
  if (r.lifecycle === "failed") {
    if (r.signal != null) return `failed · signal ${r.signal}`;
    if (r.exit_code != null) return `failed · exit ${r.exit_code}`;
  }
  if (r.lifecycle === "running" && r.health !== "ok") return `running · ${r.health}`;
  return r.lifecycle;
}

export function command(cmd: string[]): string {
  return cmd.map((a) => (a === "" || /[\s"']/.test(a) ? JSON.stringify(a) : a)).join(" ");
}

export function clock(ts: number): string {
  return new Date(ts).toLocaleTimeString(undefined, { hour12: false });
}
