<script lang="ts">
  // Notes and warn/error/info messages a run reported (::note, ::warn, parsers'
  // test failures, …): what a human should notice without reading the log.
  import { clock } from "../lib/format";

  export interface Message {
    seq: number;
    ts: number;
    level: "note" | "info" | "warn" | "error";
    text: string;
  }

  let { messages }: { messages: Message[] } = $props();
  let expanded = $state(false);
  const LIMIT = 8;
  const shown = $derived(expanded ? messages : messages.slice(-LIMIT));
</script>

{#if messages.length}
  <section class="messages">
    {#if messages.length > LIMIT}
      <button class="more" onclick={() => (expanded = !expanded)}>
        {expanded ? "Show recent only" : `Show all ${messages.length}`}
      </button>
    {/if}
    {#each shown as m (m.seq)}
      <div class="msg {m.level}">
        <span class="tag">{m.level}</span>
        <span class="text">{m.text}</span>
        <span class="time mono muted">{clock(m.ts)}</span>
      </div>
    {/each}
  </section>
{/if}

<style>
  .messages {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 6px 0;
  }
  .msg {
    display: grid;
    grid-template-columns: 56px 1fr auto;
    gap: 10px;
    padding: 3px 12px;
    align-items: baseline;
  }
  .tag {
    font-size: 11px;
    text-transform: uppercase;
    font-weight: 600;
    letter-spacing: 0.03em;
  }
  .note .tag {
    color: var(--accent);
  }
  .info .tag {
    color: var(--muted);
  }
  .warn .tag {
    color: var(--warn);
  }
  .error .tag,
  .error .text {
    color: var(--bad);
  }
  .note .text {
    font-weight: 500;
  }
  .time {
    font-size: 11px;
  }
  .more {
    margin: 2px 12px 6px;
  }
</style>
