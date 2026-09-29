<script lang="ts" module>
  export interface ViewLine {
    seq: number;
    stream: "stdout" | "stderr";
    html: string;
    text: string;
    cr: boolean;
  }
</script>

<script lang="ts">
  import { tick } from "svelte";

  let {
    lines,
    canLoadEarlier,
    onLoadEarlier,
  }: { lines: ViewLine[]; canLoadEarlier: boolean; onLoadEarlier: () => void } = $props();

  let box: HTMLDivElement;
  let follow = $state(true);
  let filter = $state("");
  let showSeq = $state(false);

  const shown = $derived.by(() => {
    const f = filter.trim().toLowerCase();
    return f ? lines.filter((l) => l.text.toLowerCase().includes(f)) : lines;
  });

  // Stick to the bottom while following; stop following when the user scrolls up.
  $effect(() => {
    void shown.length;
    void shown[shown.length - 1]?.html;
    if (follow) tick().then(() => box && (box.scrollTop = box.scrollHeight));
  });

  function onScroll() {
    const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 24;
    follow = atBottom;
  }
</script>

<div class="logbar">
  <input placeholder="Filter lines" bind:value={filter} />
  <label><input type="checkbox" bind:checked={showSeq} /> seq</label>
  <div class="spacer"></div>
  <span class="muted">{shown.length}{filter ? ` of ${lines.length}` : ""} lines</span>
  <button
    class:on={follow}
    onclick={() => {
      follow = !follow;
      if (follow) box.scrollTop = box.scrollHeight;
    }}>{follow ? "Following" : "Follow"}</button
  >
</div>
<div class="log mono" bind:this={box} onscroll={onScroll}>
  {#if canLoadEarlier}
    <button class="earlier" onclick={onLoadEarlier}>Load earlier lines</button>
  {/if}
  {#each shown as l (l.seq)}
    <div class="line" class:stderr={l.stream === "stderr"} class:cr={l.cr}>
      {#if showSeq}<span class="seq">{l.seq}</span>{/if}<span class="text">{@html l.html || " "}</span>
    </div>
  {/each}
  {#if lines.length === 0}
    <div class="faint">No output yet.</div>
  {/if}
</div>

<style>
  .logbar {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-bottom: none;
    border-radius: 10px 10px 0 0;
    background: var(--panel-2);
  }
  .logbar input:not([type]) {
    width: 260px;
  }
  .logbar label {
    display: flex;
    gap: 4px;
    align-items: center;
    color: var(--muted);
  }
  .spacer {
    flex: 1;
  }
  button.on {
    color: var(--accent);
    border-color: var(--accent);
  }
  .log {
    height: min(62vh, 640px);
    min-height: 320px;
    overflow: auto;
    padding: 8px 0;
    background: var(--log-bg);
    border: 1px solid var(--border);
    border-radius: 0 0 10px 10px;
  }
  .line {
    display: flex;
    padding: 0 12px;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .line:hover {
    background: var(--panel-2);
  }
  .stderr .text {
    color: var(--stderr);
  }
  .cr {
    opacity: 0.75;
  }
  .seq {
    flex: none;
    width: 5.5em;
    color: var(--faint);
    user-select: none;
  }
  .earlier {
    margin: 4px 12px 8px;
  }
</style>
