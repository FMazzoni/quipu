<script lang="ts">
  import {
    data, filteredTasks, indexes, rootTasks,
    selected, expanded, tagFilter, search, toggleTag, toggleExpanded,
  } from '../stores/ui';
  import { IN_FLIGHT, type Task } from '../lib/data';
  import { classifyTags } from '../lib/classify';
  import TreeNode from './TreeNode.svelte';

  // Top tags = tags appearing ≥2 times, excluding commit:* (ported from mockup).
  let topTags = $derived.by(() => {
    const m = new Map<string, number>();
    for (const t of $data.tasks) {
      for (const tag of t.tags || []) m.set(tag, (m.get(tag) || 0) + 1);
    }
    return [...m.entries()]
      .filter(([t, n]) => n >= 2 && !t.startsWith('commit:'))
      .sort((a, b) => b[1] - a[1])
      .slice(0, 12);
  });

  let visibleSet = $derived(new Set($filteredTasks.map((t) => t.display_id)));

  function onSearchInput(e: Event) {
    search.set((e.currentTarget as HTMLInputElement).value.trim());
  }
</script>

<div class="rail-header">
  Tasks <span class="count">{$filteredTasks.length}/{$data.tasks.length}</span>
</div>

<input class="searchbar" placeholder="search title or description…"
       value={$search} oninput={onSearchInput} />

<div class="tag-facets">
  {#each topTags as [tag, n]}
    <button class="facet" class:active={$tagFilter.has(tag)} onclick={() => toggleTag(tag)}>
      <span>{tag}</span><span class="n">{n}</span>
    </button>
  {/each}
</div>

<div class="task-tree">
  {#if $rootTasks.length === 0}
    <div class="empty-row">no tasks loaded</div>
  {:else}
    {#each $rootTasks as root (root.display_id)}
      <TreeNode
        task={root}
        depth={0}
        visible={visibleSet}
        indexes={$indexes}
        selected={$selected}
        expanded={$expanded}
        onselect={(id: string) => selected.set(id)}
        ontoggle={(id: string) => toggleExpanded(id)}
      />
    {/each}
    {#if $filteredTasks.length === 0}
      <div class="empty-row">no tasks match current filters</div>
    {/if}
  {/if}
</div>

<style>
  .rail-header {
    padding: 14px 16px 8px; font-size: 11px; text-transform: uppercase; letter-spacing: 0.7px;
    color: var(--muted); font-weight: 600;
    display: flex; justify-content: space-between; align-items: center;
  }
  .rail-header .count { color: var(--muted-2); font-variant-numeric: tabular-nums; }
  .searchbar {
    margin: 0 12px 8px;
    background: var(--surface-2); border: 1px solid var(--border); border-radius: 6px;
    padding: 6px 12px; color: var(--text); font-size: 13px;
  }
  .searchbar:focus { outline: none; border-color: var(--accent); }
  .tag-facets { padding: 4px 12px 12px; display: flex; flex-wrap: wrap; gap: 4px; border-bottom: 1px solid var(--border-soft); }
  .facet {
    padding: 3px 8px; border-radius: 10px; background: transparent; border: 1px solid var(--border);
    font-size: 11px; color: var(--muted); font-family: ui-monospace, monospace; transition: all 0.1s;
  }
  .facet:hover { background: var(--surface-2); color: var(--text); }
  .facet.active { background: var(--accent); color: var(--bg); border-color: var(--accent); }
  .facet :global(.n) { opacity: 0.6; margin-left: 4px; }
  .task-tree { padding: 6px 0; flex: 1; overflow: auto; }
  .empty-row { padding: 16px; color: var(--muted); font-size: 12px; font-style: italic; text-align: center; }
</style>
