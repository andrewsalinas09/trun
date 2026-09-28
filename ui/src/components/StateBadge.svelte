<script lang="ts">
  import type { RunSummary } from "../lib/types";
  import { stateLabel } from "../lib/format";

  let { run }: { run: RunSummary } = $props();

  const tone = $derived.by(() => {
    switch (run.lifecycle) {
      case "succeeded":
        return "ok";
      case "failed":
        return "bad";
      case "running":
        return run.health === "ok" ? "run" : run.health === "warn" ? "warn" : "bad";
      case "lost":
      case "preempted":
        return "warn";
      default:
        return "idle";
    }
  });
</script>

<span class="badge {tone}" class:pulse={run.lifecycle === "running"}>
  <span class="dot"></span>{stateLabel(run)}
</span>

<style>
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 1px 8px;
    border-radius: 999px;
    font-size: 12px;
    font-weight: 500;
    white-space: nowrap;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
  }
  .ok {
    color: var(--ok);
    background: var(--ok-bg);
  }
  .bad {
    color: var(--bad);
    background: var(--bad-bg);
  }
  .warn {
    color: var(--warn);
    background: var(--warn-bg);
  }
  .run {
    color: var(--run);
    background: var(--run-bg);
  }
  .idle {
    color: var(--idle);
    background: var(--idle-bg);
  }
  .pulse .dot {
    animation: pulse 1.6s ease-in-out infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.3;
    }
  }
</style>
