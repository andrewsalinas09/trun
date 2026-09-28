<script lang="ts">
  import RunList from "./components/RunList.svelte";
  import RunView from "./components/RunView.svelte";

  // Hash routing: #/ is the fleet list, #/runs/<id> is one run.
  let hash = $state(location.hash);
  $effect(() => {
    const onHash = () => (hash = location.hash);
    addEventListener("hashchange", onHash);
    return () => removeEventListener("hashchange", onHash);
  });
  const runId = $derived(hash.match(/^#\/runs\/([^/?]+)/)?.[1] ?? null);
</script>

<header>
  <a class="brand" href="#/">trun</a>
  {#if runId}
    <span class="crumb">/ run</span>
  {/if}
</header>

<main>
  {#if runId}
    {#key runId}
      <RunView id={runId} />
    {/key}
  {:else}
    <RunList />
  {/if}
</main>

<style>
  header {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 10px 20px;
    border-bottom: 1px solid var(--border);
    background: var(--panel);
    position: sticky;
    top: 0;
    z-index: 10;
  }
  .brand {
    font-weight: 700;
    font-size: 16px;
    color: var(--text);
    letter-spacing: -0.01em;
  }
  .crumb {
    color: var(--muted);
  }
  main {
    padding: 16px 20px 40px;
    max-width: 1400px;
    margin: 0 auto;
  }
</style>
