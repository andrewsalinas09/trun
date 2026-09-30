<script lang="ts">
  // Remote hosts at a glance: connection state, platform, clock skew, errors.
  import { api } from "../lib/api";
  import { ago, duration } from "../lib/format";
  import { now } from "../lib/now.svelte";
  import type { HostState } from "../lib/types";

  let hosts = $state<HostState[]>([]);

  $effect(() => {
    let alive = true;
    const load = () =>
      api
        .hosts()
        .then((h) => alive && (hosts = h))
        .catch(() => {});
    load();
    const t = setInterval(load, 3000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  });
</script>

{#if hosts.length}
  <div class="hosts">
    <span class="label muted">Hosts</span>
    <span class="host local"><span class="dot online"></span>local</span>
    {#each hosts as h (h.name)}
      <span
        class="host"
        title={[
          `${h.ssh.join(" ")} ${h.target}`,
          h.info ? `${h.info.hostname} · ${h.info.os}/${h.info.arch} · trun ${h.info.version}` : "",
          h.last_seen ? `seen ${ago(h.last_seen, now.value)}` : "",
          h.clock_offset_ms && Math.abs(h.clock_offset_ms) >= 1000
            ? `clock ${duration(Math.abs(h.clock_offset_ms))} ${h.clock_offset_ms > 0 ? "ahead" : "behind"} (adjusted)`
            : "",
          h.error ?? "",
        ]
          .filter(Boolean)
          .join("\n")}
      >
        <span class="dot {h.status}"></span>{h.name}
        {#if h.active_runs}<span class="count">{h.active_runs}</span>{/if}
        {#if h.status === "offline" && h.error}<span class="err">{h.error}</span>{/if}
      </span>
    {/each}
  </div>
{/if}

<style>
  .hosts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin-bottom: 12px;
  }
  .label {
    font-size: 12px;
    margin-right: 2px;
  }
  .host {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 10px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--panel);
    font-size: 13px;
    max-width: 520px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--idle);
  }
  .dot.online {
    background: var(--ok);
  }
  .dot.connecting {
    background: var(--warn);
    animation: pulse 1.2s infinite;
  }
  .dot.offline {
    background: var(--bad);
  }
  @keyframes pulse {
    50% {
      opacity: 0.3;
    }
  }
  .count {
    font-size: 11px;
    padding: 0 6px;
    border-radius: 999px;
    background: var(--run-bg);
    color: var(--run);
  }
  .err {
    color: var(--bad);
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
