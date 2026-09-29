<script lang="ts">
  // Open check alerts: why this run is not healthy right now.
  import { ago } from "../lib/format";
  import { now } from "../lib/now.svelte";
  import type { Alert } from "../lib/types";

  let { alerts }: { alerts: Alert[] } = $props();
</script>

{#if alerts.length}
  <section class="alerts">
    {#each alerts as a (a.key)}
      <div class="alert {a.level}">
        <span class="level">{a.level}</span>
        <span class="msg">{a.message}</span>
        <span class="meta muted">since {ago(a.opened_at, now.value).replace(" ago", "")} · {a.check}</span>
      </div>
    {/each}
  </section>
{/if}

<style>
  .alerts {
    display: grid;
    gap: 6px;
    margin: 12px 0;
  }
  .alert {
    display: grid;
    grid-template-columns: 70px 1fr auto;
    gap: 10px;
    align-items: baseline;
    padding: 8px 12px;
    border-radius: 10px;
    border: 1px solid var(--border);
    background: var(--panel);
  }
  .level {
    font-size: 11px;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .fail {
    background: var(--bad-bg);
    border-color: color-mix(in srgb, var(--bad) 40%, transparent);
  }
  .fail .level {
    color: var(--bad);
  }
  .stalled,
  .warn {
    background: var(--warn-bg);
    border-color: color-mix(in srgb, var(--warn) 40%, transparent);
  }
  .stalled .level,
  .warn .level {
    color: var(--warn);
  }
  .info .level {
    color: var(--muted);
  }
  .msg {
    font-weight: 500;
  }
  .meta {
    font-size: 12px;
    white-space: nowrap;
  }
</style>
