// Thin client for the hub API. Auth is the HttpOnly cookie the hub sets when the
// UI is opened via `trun ui` (`/?t=<token>`), so fetch and EventSource just work.

import type { HostState, LogPage, MetricSeries, ProjectInfo, RunEvent, RunSummary } from "./types";

export class Unauthorized extends Error {}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`/api${path}`, { credentials: "same-origin", ...init });
  if (res.status === 401) throw new Unauthorized("not authorized");
  if (!res.ok) {
    let msg = `${res.status} ${res.statusText}`;
    try {
      msg = (await res.json()).error ?? msg;
    } catch {
      /* not json */
    }
    throw new Error(msg);
  }
  return (await res.json()) as T;
}

export const api = {
  runs: (q: { active?: boolean; project?: string; limit?: number } = {}) => {
    const p = new URLSearchParams();
    if (q.active) p.set("active", "true");
    if (q.project) p.set("project", q.project);
    p.set("limit", String(q.limit ?? 200));
    return request<RunSummary[]>(`/runs?${p}`);
  },
  run: (id: string) => request<RunSummary>(`/runs/${encodeURIComponent(id)}`),
  logs: (id: string, q: { tail?: number; before_seq?: number; since_seq?: number }) => {
    const p = new URLSearchParams();
    for (const [k, v] of Object.entries(q)) if (v !== undefined) p.set(k, String(v));
    return request<LogPage>(`/runs/${encodeURIComponent(id)}/logs?${p}`);
  },
  projects: () => request<ProjectInfo[]>("/projects"),
  hosts: () => request<HostState[]>("/hosts"),
  messages: (id: string) => request<RunEvent[]>(`/runs/${encodeURIComponent(id)}/messages`),
  metrics: (id: string, names?: string[], maxPoints = 1500) => {
    const p = new URLSearchParams({ max_points: String(maxPoints) });
    if (names) p.set("names", names.join(","));
    return request<MetricSeries[]>(`/runs/${encodeURIComponent(id)}/metrics?${p}`);
  },
  cancel: (id: string, force = false) =>
    request<RunSummary>(`/runs/${encodeURIComponent(id)}/cancel`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ force }),
    }),
};

/** Subscribe to an SSE endpoint; returns a close function. EventSource reconnects
 *  on its own, resuming via Last-Event-ID. */
export function subscribe(
  path: string,
  handlers: Record<string, (data: unknown) => void>,
  onError?: () => void,
): () => void {
  const es = new EventSource(`/api${path}`, { withCredentials: true });
  for (const [event, fn] of Object.entries(handlers)) {
    es.addEventListener(event, (e) => fn(JSON.parse((e as MessageEvent).data)));
  }
  if (onError) es.onerror = onError;
  return () => es.close();
}
