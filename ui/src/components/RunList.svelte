<script lang="ts">
  import { api, subscribe, Unauthorized } from "../lib/api";
  import { ago, duration, elapsed, headline } from "../lib/format";
  import { now } from "../lib/now.svelte";
  import { TERMINAL, type RunSummary } from "../lib/types";
  import AuthNotice from "./AuthNotice.svelte";
  import StateBadge from "./StateBadge.svelte";

  let runs = $state<Record<string, RunSummary>>({});
  let project = $state("");
  let activeOnly = $state(false);
  let error = $state<string | null>(null);
  let unauthorized = $state(false);

  $effect(() => {
    api
      .runs({ limit: 300 })
      .then((list) => {
        const next: Record<string, RunSummary> = {};
        for (const r of list) next[r.id] = r;
        runs = next;
      })
      .catch((e) => {
        if (e instanceof Unauthorized) unauthorized = true;
        else error = String(e.message ?? e);
      });
    return subscribe("/stream", {
      run: (d) => {
        const r = d as RunSummary & { type: string };
        runs[r.id] = r;
      },
    });
  });

  const projects = $derived([...new Set(Object.values(runs).map((r) => r.project))].sort());
  const visible = $derived(
    Object.values(runs)
      .filter((r) => (!project || r.project === project) && (!activeOnly || !TERMINAL.has(r.lifecycle)))
      .sort((a, b) => b.created_at - a.created_at),
  );
  const activeCount = $derived(Object.values(runs).filter((r) => !TERMINAL.has(r.lifecycle)).length);
</script>

{#if unauthorized}
  <AuthNotice />
{:else}
  <div class="toolbar">
    <h1>Runs</h1>
    <span class="muted">{activeCount} active</span>
    <div class="spacer"></div>
    <label><input type="checkbox" bind:checked={activeOnly} /> active only</label>
    <select bind:value={project}>
      <option value="">all projects</option>
      {#each projects as p (p)}
        <option value={p}>{p}</option>
      {/each}
    </select>
  </div>

  {#if error}
    <p class="error">{error}</p>
  {/if}

  {#if visible.length === 0}
    <div class="empty">
      <p>No runs yet.</p>
      <p class="muted">Start one with <code>trun run -- &lt;command&gt;</code>.</p>
    </div>
  {:else}
    <table>
      <thead>
        <tr>
          <th>Name</th>
          <th>State</th>
          <th>Progress</th>
          <th>Project</th>
          <th>Host</th>
          <th class="num">Duration</th>
          <th>Last output</th>
          <th>Started</th>
        </tr>
      </thead>
      <tbody>
        {#each visible as r (r.id)}
          {@const el = elapsed(r, now.value)}
          <tr onclick={() => (location.hash = `#/runs/${r.id}`)}>
            <td>
              <a href="#/runs/{r.id}" class="name">{r.name}</a>
              {#if r.diagnosis}
                <div class="diag" title={r.diagnosis.summary}>{r.diagnosis.summary}</div>
              {/if}
            </td>
            <td><StateBadge run={r} /></td>
            <td class="progress">
              {#if !TERMINAL.has(r.lifecycle) && headline(r)}
                {@const h = headline(r)!}
                <div class="pbar" title={h.label}>{#if h.pct != null}<span style:width="{h.pct}%"></span>{:else}<i></i>{/if}</div>
                <div class="plabel muted">{h.label}</div>
              {/if}
            </td>
            <td class="muted">{r.project}</td>
            <td class="muted">{r.host}</td>
            <td class="num mono">{el == null ? "–" : duration(el)}</td>
            <td class="muted">
              {#if r.last_output_at}
                {TERMINAL.has(r.lifecycle) ? "–" : ago(r.last_output_at, now.value)}
              {:else}
                <span class="faint">none</span>
              {/if}
            </td>
            <td class="muted">{ago(r.created_at, now.value)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
{/if}

<style>
  .toolbar {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 12px;
  }
  h1 {
    font-size: 18px;
    margin: 0;
  }
  .spacer {
    flex: 1;
  }
  label {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--muted);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    overflow: hidden;
  }
  th {
    text-align: left;
    font-weight: 500;
    font-size: 12px;
    color: var(--muted);
    padding: 8px 12px;
    background: var(--panel-2);
    border-bottom: 1px solid var(--border);
  }
  td {
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
    vertical-align: top;
  }
  tbody tr {
    cursor: pointer;
  }
  tbody tr:hover {
    background: var(--panel-2);
  }
  tbody tr:last-child td {
    border-bottom: none;
  }
  .num {
    text-align: right;
  }
  .name {
    font-weight: 500;
    color: var(--text);
  }
  .diag {
    color: var(--bad);
    font-size: 12px;
    max-width: 520px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .empty {
    text-align: center;
    padding: 60px 0;
  }
  .progress {
    width: 170px;
  }
  .pbar {
    position: relative;
    height: 5px;
    border-radius: 3px;
    background: var(--panel-2);
    overflow: hidden;
    margin-top: 6px;
  }
  .pbar span {
    position: absolute;
    inset: 0 auto 0 0;
    background: var(--run);
    transition: width 0.3s;
  }
  .pbar i {
    position: absolute;
    inset: 0;
    width: 30%;
    background: var(--run);
    opacity: 0.5;
    animation: slide 1.4s ease-in-out infinite;
  }
  @keyframes slide {
    from {
      left: -30%;
    }
    to {
      left: 100%;
    }
  }
  .plabel {
    font-size: 11px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 170px;
  }
  .error {
    color: var(--bad);
  }
</style>
