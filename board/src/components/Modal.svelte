<script lang="ts">
  import { modalOpen, closeModal } from '../stores/modal';
  import { data, filteredTasks, indexes, selected } from '../stores/ui';
  import { buildGraphData, drawGraph, type DrawHandle } from '../lib/graph';

  let svgEl: SVGSVGElement | undefined = $state();
  let handle: DrawHandle | null = null;

  $effect(() => {
    if (!$modalOpen) {
      if (handle) { handle.destroy(); handle = null; }
      return;
    }
    if (!svgEl) return;
    if (handle) { handle.destroy(); handle = null; }
    const ids = $filteredTasks.map((t) => t.display_id);
    const { nodes, links } = buildGraphData(ids, $indexes, $data.deps);
    handle = drawGraph(svgEl, nodes, links, {
      linkDistance: 90,
      charge: -350,
      sticky: true,
      selected: $selected,
      onSelect: (id) => selected.set(id),
    });
  });

  function onBackdrop(e: MouseEvent) {
    if (e.target === e.currentTarget) closeModal();
  }
</script>

{#if $modalOpen}
  <div class="modal-overlay open" onclick={onBackdrop} role="dialog" tabindex="-1">
    <div class="modal">
      <div class="modal-header">
        <span class="title">Full graph — <span>{$filteredTasks.length} matching task{$filteredTasks.length === 1 ? '' : 's'}</span></span>
        <button class="btn-sm" onclick={closeModal}>close ✕</button>
      </div>
      <div class="modal-body">
        <svg bind:this={svgEl}></svg>
      </div>
    </div>
  </div>
{/if}

<style>
  .modal-overlay { position: fixed; inset: 0; background: rgba(0,0,0,0.88); display: flex; align-items: center; justify-content: center; z-index: 100; }
  .modal { background: var(--surface); border: 1px solid var(--border); border-radius: 8px; width: 92vw; height: 88vh; display: flex; flex-direction: column; overflow: hidden; }
  .modal-header { padding: 14px 20px; border-bottom: 1px solid var(--border); display: flex; align-items: center; gap: 12px; }
  .modal-header .title { font-weight: 600; flex: 1; }
  .modal-body { flex: 1; overflow: hidden; position: relative; }
  .modal-body svg { width: 100%; height: 100%; display: block; cursor: grab; }
  .modal-body svg:active { cursor: grabbing; }
  .btn-sm { background: var(--surface-2); border: 1px solid var(--border); color: var(--muted); padding: 4px 10px; border-radius: 4px; font-size: 11px; }
  .btn-sm:hover { background: var(--surface-3); color: var(--text); }
</style>
