<script lang="ts">
  import { onDestroy } from 'svelte';
  import { data, indexes, selected } from '../stores/ui';
  import { buildGraphData, drawGraph, type DrawHandle } from '../lib/graph';
  import { openModal } from '../stores/modal';

  let svgEl: SVGSVGElement | undefined = $state();
  let handle: DrawHandle | null = null;

  // Neighborhood ids: selected + 1-hop parents + 1-hop children + 2-hop on each side.
  let neighborhood = $derived.by(() => {
    if (!$selected) return [] as string[];
    const idx = $indexes;
    const sel = $selected;
    const p1 = idx.parentsOf.get(sel) || [];
    const c1 = idx.childrenOf.get(sel) || [];
    const p2 = new Set<string>();
    for (const p of p1) for (const g of idx.parentsOf.get(p) || []) p2.add(g);
    const c2 = new Set<string>();
    for (const c of c1) for (const g of idx.childrenOf.get(c) || []) c2.add(g);
    return [...new Set([...p2, ...p1, sel, ...c1, ...c2])];
  });

  $effect(() => {
    // Re-render whenever neighborhood or svg changes.
    if (!svgEl) return;
    if (handle) { handle.destroy(); handle = null; }
    if (neighborhood.length < 2) return;
    const { nodes, links } = buildGraphData(neighborhood, $indexes, $data.deps);
    handle = drawGraph(svgEl, nodes, links, {
      linkDistance: 70,
      charge: -220,
      selected: $selected,
      onSelect: (id) => selected.set(id),
    });
  });

  onDestroy(() => { handle?.destroy(); });

  function onReset() { handle?.reset(); }
  function onFull() { openModal(); }
</script>

<div class="dag-toolbar">
  <span class="label">Local graph</span>
  <button class="btn-sm" onclick={onReset}>reset ⟲</button>
  <button class="btn-sm" onclick={onFull}>Full ↗</button>
</div>

<div class="graph-container">
  {#if !$selected}
    <div class="graph-empty">select a ticket to see its neighborhood</div>
  {:else if neighborhood.length < 2}
    <div class="graph-empty"><strong>{$selected}</strong> has no neighbors</div>
  {:else}
    <svg bind:this={svgEl}></svg>
    <div class="graph-hint">drag to move • scroll to zoom • click node to select</div>
  {/if}
</div>

<style>
  .dag-toolbar { padding: 12px 16px; border-bottom: 1px solid var(--border-soft); display: flex; gap: 8px; align-items: center; }
  .dag-toolbar .label { color: var(--muted); font-size: 11px; text-transform: uppercase; letter-spacing: 0.7px; font-weight: 600; flex: 1; }
  .btn-sm { background: var(--surface-2); border: 1px solid var(--border); color: var(--muted); padding: 4px 10px; border-radius: 4px; font-size: 11px; transition: all 0.1s; }
  .btn-sm:hover { background: var(--surface-3); color: var(--text); border-color: var(--muted-2); }
  .graph-container { flex: 1; position: relative; overflow: hidden; }
  .graph-container svg { width: 100%; height: 100%; display: block; cursor: grab; }
  .graph-container svg:active { cursor: grabbing; }
  .graph-empty { color: var(--muted); padding: 60px 20px; text-align: center; font-size: 13px; }
  .graph-hint { position: absolute; bottom: 8px; left: 12px; color: var(--muted-2); font-size: 10px; pointer-events: none; }

  :global(.node-circle) { stroke-width: 1.5px; cursor: pointer; transition: opacity 0.15s; }
  :global(.node-circle.selected) { stroke-width: 3px; }
  :global(.node-label) { fill: var(--text); font-family: ui-monospace, monospace; font-size: 9px; pointer-events: none; opacity: 0.8; }
  :global(.node-label.large) { font-size: 11px; opacity: 1; font-weight: 600; }
  :global(.link) { stroke: var(--muted-2); stroke-opacity: 0.5; stroke-width: 1px; transition: stroke 0.15s, opacity 0.15s; fill: none; }
  :global(.link.highlighted) { stroke: var(--accent); stroke-opacity: 1; stroke-width: 2px; }
  :global(.dim) { opacity: 0.18; }
</style>
