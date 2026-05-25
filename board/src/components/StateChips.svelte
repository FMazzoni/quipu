<script lang="ts">
  import { data, stateFilter, toggleState } from '../stores/ui';
  import { STATES, type State } from '../lib/data';

  // Tally state counts whenever data updates.
  let counts = $derived.by(() => {
    const c: Record<State, number> = {
      pending: 0, ready: 0, assigned: 0,
      running: 0, done: 0, cancelled: 0,
    };
    for (const t of $data.tasks) c[t.state] = (c[t.state] || 0) + 1;
    return c;
  });
</script>

<div class="state-chips">
  {#each STATES as s}
    <button
      class="state-chip"
      class:active={$stateFilter.has(s)}
      data-s={s}
      onclick={() => toggleState(s)}
    >
      <span class="dot"></span>
      <span>{s}</span>
      <span class="n">{counts[s]}</span>
    </button>
  {/each}
</div>

<style>
  .state-chips { display: flex; gap: 4px; }
  .state-chip {
    display: inline-flex; align-items: center; gap: 6px;
    padding: 4px 10px; border-radius: 12px; background: transparent; color: var(--muted);
    font-size: 12px; user-select: none; border: 1px solid var(--border); transition: all 0.12s;
  }
  .state-chip:hover { background: var(--surface-2); color: var(--text); }
  .state-chip.active { background: var(--surface-3); color: var(--text); border-color: currentColor; }
  .state-chip .dot { width: 6px; height: 6px; border-radius: 50%; background: currentColor; }
  .state-chip[data-s="ready"]    { --c: var(--s-ready); }
  .state-chip[data-s="pending"]  { --c: var(--s-pending); }
  .state-chip[data-s="assigned"] { --c: var(--s-assigned); }
  .state-chip[data-s="running"]  { --c: var(--s-running); }
  .state-chip[data-s="done"]     { --c: var(--s-done); }
  .state-chip[data-s="cancelled"]{ --c: var(--s-cancelled); }
  .state-chip.active { color: var(--c); }
  .state-chip .n { color: var(--muted); font-variant-numeric: tabular-nums; font-size: 11px; }
  .state-chip.active .n { color: var(--text); }
</style>
