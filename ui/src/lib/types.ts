// Mirrors crates/trun-proto/src/{lib,structure}.rs and crates/trun-hub/src/panels.rs.

export type Lifecycle =
  | "queued"
  | "starting"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "lost"
  | "preempted";

export type Health = "ok" | "warn" | "stalled" | "failing";
export type StreamName = "stdout" | "stderr";

/** Metric values: finite numbers, or "NaN" / "inf" / "-inf" (JSON has no literal). */
export type WireNum = number | "NaN" | "inf" | "-inf";

export function num(v: WireNum): number {
  if (typeof v === "number") return v;
  if (v === "inf") return Infinity;
  if (v === "-inf") return -Infinity;
  return NaN;
}

export interface EvidenceLine {
  seq: number;
  text: string;
}

export interface Diagnosis {
  cause: string;
  summary: string;
  early: boolean;
  evidence: EvidenceLine[];
}

export type StepRunState = "running" | "ok" | "failed" | "skipped";

export interface StepState {
  id: string;
  parent: string | null;
  name: string;
  state: StepRunState;
  current: number | null;
  total: number | null;
  unit: string | null;
  rate: number | null;
  eta_ms: number | null;
  started_at: number;
  ended_at: number | null;
  progressed_at: number | null;
}

export type AlertLevel = "info" | "warn" | "stalled" | "fail";

export interface Alert {
  key: string;
  check: string;
  level: AlertLevel;
  message: string;
  opened_at: number;
  last_at: number;
  count: number;
}

export const LEVEL_RANK: Record<AlertLevel, number> = { info: 0, warn: 1, stalled: 2, fail: 3 };

export function healthOf(alerts: Alert[]): Health {
  const worst = alerts.reduce((m, a) => Math.max(m, LEVEL_RANK[a.level]), 0);
  return (["ok", "warn", "stalled", "failing"] as const)[worst];
}

export interface MetricLast {
  value: WireNum;
  step: number | null;
  ts: number;
  count: number;
  non_finite: number;
}

export interface RunSummary {
  id: string;
  project: string;
  name: string;
  host: string;
  cmd: string[];
  cwd: string;
  lifecycle: Lifecycle;
  health: Health;
  created_at: number;
  started_at: number | null;
  ended_at: number | null;
  exit_code: number | null;
  signal: number | null;
  pid: number | null;
  diagnosis: Diagnosis | null;
  last_seq: number;
  last_output_at: number | null;
  last_activity_at: number | null;
  steps: StepState[];
  metrics: Record<string, MetricLast>;
  expect_silence_ms: number | null;
  config_root: string | null;
  pty: boolean;
  alerts: Alert[];
}

export interface LogLine {
  seq: number;
  ts: number;
  stream: StreamName;
  text: string;
  cr: boolean;
}

export interface LogPage {
  lines: LogLine[];
  next_seq: number;
}

export type RunEvent = { run_id: string; seq: number; ts: number } & (
  | { kind: "output"; stream: StreamName; text: string; cr: boolean }
  | {
      kind: "lifecycle";
      state: Lifecycle;
      exit_code?: number;
      signal?: number;
      pid?: number;
      reason?: string;
    }
  | ({ kind: "diagnosis" } & Diagnosis)
  | { kind: "step_begin"; id: string; name: string; parent?: string }
  | { kind: "step_end"; id: string; status: "ok" | "failed" | "skipped" }
  | {
      kind: "progress";
      id: string;
      current: number;
      total?: number;
      unit?: string;
      rate?: number;
      eta_ms?: number;
    }
  | { kind: "metric"; values: Record<string, WireNum>; step?: number }
  | { kind: "log"; level: "info" | "warn" | "error"; text: string }
  | { kind: "note"; text: string; author: string }
  | {
      kind: "alert";
      key: string;
      check: string;
      level: AlertLevel;
      message: string;
      state: "opened" | "updated" | "cleared";
    }
  | { kind: "check_error"; check: string; error: string }
);

export interface MetricPoint {
  s?: number;
  t: number;
  v: WireNum;
}

export interface MetricSeries {
  name: string;
  points: MetricPoint[];
  total: number;
  downsampled: boolean;
}

export interface HostState {
  name: string;
  target: string;
  ssh: string[];
  trun_path: string;
  status: "connecting" | "online" | "offline";
  error: string | null;
  info: { hostname: string; os: string; arch: string; version: string } | null;
  last_seen: number | null;
  active_runs: number;
  clock_offset_ms: number | null;
}

export interface ProjectInfo {
  name: string;
  run_count: number;
  active_count: number;
  last_run_at: number | null;
}

// ---- panels (crates/trun-hub/src/panels.rs) ----

export type PanelKind = "line" | "area" | "bar" | "scatter" | "stat" | "table";

export interface Threshold {
  below?: number | null;
  above?: number | null;
  color: string;
}

export interface PanelSpec {
  title?: string | null;
  kind: PanelKind;
  source: string;
  y: string | string[];
  x: "step" | "time" | "elapsed";
  smooth?: number | null;
  scale?: { y?: string | null } | null;
  runs?: string | null;
  reduce?: string | null;
  unit?: string | null;
  thresholds: Threshold[];
  height?: number | null;
}

export interface Cell {
  panel: string;
  span?: number | null;
  height?: number | null;
}

export interface DashboardSpec {
  title?: string | null;
  applies?: string | null;
  columns?: number | null;
  row: { panels: Cell[] }[];
}

export interface Loaded<T> {
  name: string;
  file: string;
  scope: "project" | "global";
  spec: T | null;
  error: string | null;
}

export interface PanelSet {
  panels: Loaded<PanelSpec>[];
  dashboards: Loaded<DashboardSpec>[];
  dashboard: string | null;
  dirs: string[];
}

export const TERMINAL: ReadonlySet<Lifecycle> = new Set([
  "succeeded",
  "failed",
  "cancelled",
  "lost",
  "preempted",
]);

export function yNames(spec: PanelSpec): string[] {
  return Array.isArray(spec.y) ? spec.y : [spec.y];
}
