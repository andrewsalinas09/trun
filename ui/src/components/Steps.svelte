<script lang="ts">
  import { duration } from "../lib/format";
  import { fmtNum } from "../lib/metrics.svelte";
  import { now } from "../lib/now.svelte";
  import type { StepState } from "../lib/types";

  let { steps }: { steps: StepState[] } = $props();

  /** Sibling lists longer than this collapse their finished-ok members. */
  const COLLAPSE_OVER = 8;
  const KEEP_RECENT = 3;
  let expanded = $state<Record<string, boolean>>({});

  type Row = { kind: "step"; step: StepState; depth: number } | { kind: "more"; parent: string; hidden: number; depth: number };

  // Parents first, children right after their parent, in begin order. Long runs of
  // finished siblings (60 epochs…) fold into one "+N more" row; running and failed
  // steps are always shown.
  const rows = $derived.by(() => {
    const out: Row[] = [];
    const seen = new Set<string>();
    const visit = (parent: string | null, depth: number) => {
      const kids = steps.filter((s) => (s.parent ?? null) === parent && !seen.has(s.id));
      const key = parent ?? "";
      const collapse = kids.length > COLLAPSE_OVER && !expanded[key];
      let hidden = 0;
      kids.forEach((s, i) => {
        seen.add(s.id);
        const recent = i >= kids.length - KEEP_RECENT;
        if (collapse && !recent && s.state === "ok") {
          hidden++;
          return;
        }
        if (hidden) {
          out.push({ kind: "more", parent: key, hidden, depth });
          hidden = 0;
        }
        out.push({ kind: "step", step: s, depth });
        visit(s.id, depth + 1);
      });
      if (hidden) out.push({ kind: "more", parent: key, hidden, depth });
    };
    visit(null, 0);
    for (const s of steps) if (!seen.has(s.id)) out.push({ kind: "step", step: s, depth: 0 });
    return out;
  });

  function pct(s: StepState): number | null {
    if (s.current == null || !s.total) return null;
    return Math.min(100, (s.current / s.total) * 100);
  }

  const icon: Record<string, string> = { running: "▸", ok: "✓", failed: "✗", skipped: "–" };
</script>

<div class="steps">
  {#each rows as row, i (row.kind === "step" ? row.step.id : `more-${row.parent}-${i}`)}
    {#if row.kind === "more"}
      <button class="more" style:padding-left="{row.depth * 18 + 36}px" onclick={() => (expanded[row.parent] = true)}>
        ✓ {row.hidden} more finished
      </button>
    {:else}
    {@const s = row.step}
    {@const depth = row.depth}
    {@const p = pct(s)}
    <div class="step {s.state}" style:padding-left="{depth * 18 + 10}px">
      <span class="icon">{icon[s.state]}</span>
      <span class="name" title={s.id}>{s.name}</span>
      <span class="bar">
        {#if p != null}
          <span class="fill" style:width="{p}%"></span>
        {:else if s.state === "running"}
          <span class="indeterminate"></span>
        {/if}
      </span>
      <span class="count mono">
        {#if s.current != null}
          {fmtNum(s.current)}{#if s.total}/{fmtNum(s.total)}{/if}{#if s.unit}&nbsp;{s.unit}{/if}
          {#if p != null}<span class="muted">({p.toFixed(0)}%)</span>{/if}
        {/if}
      </span>
      <span class="meta mono muted">
        {#if s.state === "running" && s.rate}{fmtNum(Number(s.rate.toPrecision(3)))}/s{/if}
        {#if s.state === "running" && s.eta_ms != null}&nbsp;· eta {duration(s.eta_ms)}{/if}
      </span>
      <span class="took mono muted">{duration((s.ended_at ?? now.value) - s.started_at)}</span>
    </div>
    {/if}
  {/each}
</div>

<style>
  .steps {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 6px 0;
  }
  .step {
    display: grid;
    grid-template-columns: 16px minmax(120px, 1.4fr) minmax(80px, 2fr) minmax(110px, auto) 150px 70px;
    align-items: center;
    gap: 10px;
    padding: 3px 12px 3px 10px;
  }
  .icon {
    text-align: center;
  }
  .more {
    display: block;
    width: 100%;
    text-align: left;
    border: none;
    background: none;
    color: var(--muted);
    font-size: 12px;
    padding-top: 2px;
    padding-bottom: 2px;
  }
  .more:hover {
    color: var(--accent);
    background: var(--panel-2);
  }
  .running .icon {
    color: var(--run);
  }
  .ok .icon {
    color: var(--ok);
  }
  .failed .icon,
  .failed .name {
    color: var(--bad);
  }
  .skipped {
    opacity: 0.6;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .bar {
    position: relative;
    height: 6px;
    border-radius: 3px;
    background: var(--panel-2);
    overflow: hidden;
  }
  .fill {
    position: absolute;
    inset: 0 auto 0 0;
    background: var(--run);
    border-radius: 3px;
    transition: width 0.3s;
  }
  .ok .fill {
    background: var(--ok);
  }
  .failed .fill {
    background: var(--bad);
  }
  .indeterminate {
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
  .count,
  .meta,
  .took {
    white-space: nowrap;
    font-size: 12px;
  }
  .took {
    text-align: right;
  }
</style>
