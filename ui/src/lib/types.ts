// Mirrors crates/trun-proto/src/lib.rs.

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
);

export interface ProjectInfo {
  name: string;
  run_count: number;
  active_count: number;
  last_run_at: number | null;
}

export const TERMINAL: ReadonlySet<Lifecycle> = new Set([
  "succeeded",
  "failed",
  "cancelled",
  "lost",
  "preempted",
]);
