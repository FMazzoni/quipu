<script lang="ts">
  import type { Task } from '../lib/data';
  import type { Indexes } from '../stores/ui';
  import { IN_FLIGHT } from '../lib/data';
  import { classifyTags } from '../lib/classify';
  import Self from './TreeNode.svelte';

  interface Props {
    task: Task;
    depth: number;
    visible: Set<string>;
    indexes: Indexes;
    selected: string | null;
    expanded: Set<string>;
    onselect: (id: string) => void;
    ontoggle: (id: string) => void;
  }

  let { task, depth, visible, indexes, selected, expanded, onselect, ontoggle }: Props = $props();

  // Direct children = tasks this one depends on (visually below).
  let childTasks = $derived.by(() => {
    const ids = indexes.childrenOf.get(task.display_id) || [];
    return ids
      .map((cid) => indexes.byId.get(cid))
      .filter((c): c is Task => !!c)
      .sort((a, b) => b.id - a.id);
  });

  let hasKids = $derived(childTasks.length > 0);
  let selfMatches = $derived(visible.has(task.display_id));

  // We render this node if it matches the filter OR a descendant does. We
  // compute that recursively via the rendered child slot existence — i.e.
  // we always include children in `childMatching` count via a quick subtree
  // scan, then suppress empty branches at render-time.
  let descendantMatches = $derived.by(() => {
    if (!hasKids) return false;
    const stack = [...(indexes.childrenOf.get(task.display_id) || [])];
    const seen = new Set<string>();
    while (stack.length) {
      const id = stack.pop()!;
      if (seen.has(id)) continue;
      seen.add(id);
      if (visible.has(id)) return true;
      for (const c of indexes.childrenOf.get(id) || []) stack.push(c);
    }
    return false;
  });

  let shouldRender = $derived(selfMatches || descendantMatches);

  let isOpen = $derived(
    hasKids && (expanded.has(task.display_id) || (depth === 0 && IN_FLIGHT.has(task.state)))
  );

  let tags = $derived(classifyTags(task.tags));
  let isRecent = $derived.by(() => {
    if (!task.last_event) return false;
    return Date.now() - new Date(task.last_event.ts).getTime() < 24 * 3600 * 1000;
  });

  function onRowClick(e: MouseEvent) {
    const target = e.target as HTMLElement;
    if (target.classList.contains('twisty')) {
      if (hasKids) ontoggle(task.display_id);
      return;
    }
    onselect(task.display_id);
  }
</script>

{#if shouldRender}
  <div class="tree-item" class:open={isOpen} class:collapsible={hasKids}>
    <div
      class="tree-row"
      class:selected={selected === task.display_id}
      data-state={task.state}
      onclick={onRowClick}
      role="button"
      tabindex="0"
    >
      {#if hasKids}
        <span class="twisty">▸</span>
      {:else}
        <span class="twisty empty">·</span>
      {/if}
      <span class="state-dot"></span>
      <span class="id">{task.display_id}</span>
      <span class="title">{task.title}</span>
      {#if tags.kind}
        <span class="badge" data-k={tags.kind}>{tags.kind}</span>
      {/if}
      {#if isRecent}
        <div class="recent" title="active in last 24h"></div>
      {/if}
    </div>
    {#if isOpen && hasKids}
      <div class="tree-children">
        {#each childTasks as child (child.display_id)}
          <Self
            task={child}
            depth={depth + 1}
            {visible}
            {indexes}
            {selected}
            {expanded}
            {onselect}
            {ontoggle}
          />
        {/each}
      </div>
    {/if}
  </div>
{/if}

<style>
  .tree-item { user-select: none; }
  .tree-item.collapsible > .tree-row { cursor: pointer; }
  .tree-row {
    padding: 7px 16px 7px 16px; display: flex; align-items: center; gap: 8px;
    font-size: 13px; border-left: 2px solid transparent; transition: background 0.08s;
  }
  .tree-row:hover { background: var(--surface-2); }
  .tree-row.selected { background: var(--surface-3); border-left-color: var(--accent); }
  .tree-row .twisty {
    width: 12px; color: var(--muted-2); font-size: 9px; flex-shrink: 0;
    display: inline-block; text-align: center; transition: transform 0.12s;
  }
  .tree-row .twisty.empty { color: transparent; }
  .tree-item.open > .tree-row > .twisty { transform: rotate(90deg); }
  .tree-row .state-dot { width: 6px; height: 6px; border-radius: 50%; flex-shrink: 0; }
  .tree-row[data-state="ready"]    .state-dot { background: var(--s-ready); }
  .tree-row[data-state="pending"]  .state-dot { background: var(--s-pending); }
  .tree-row[data-state="assigned"] .state-dot { background: var(--s-assigned); }
  .tree-row[data-state="running"]  .state-dot { background: var(--s-running); animation: pulse 1.5s ease-in-out infinite; }
  .tree-row[data-state="done"]     .state-dot { background: var(--s-done); opacity: 0.5; }
  .tree-row[data-state="cancelled"] .state-dot { background: var(--s-cancelled); opacity: 0.4; }
  @keyframes pulse { 0%,100% { opacity: 1; } 50% { opacity: 0.4; } }
  .tree-row .id { font-family: ui-monospace, monospace; font-size: 11px; color: var(--muted); min-width: 44px; }
  .tree-row .title { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text); }
  .tree-row .badge {
    padding: 1px 5px; border-radius: 3px; font-size: 9px; text-transform: uppercase; letter-spacing: 0.5px;
    background: var(--surface-3); color: var(--muted); font-weight: 600; flex-shrink: 0;
  }
  .tree-row .badge[data-k="wave"] { background: rgba(167,139,250,0.18); color: var(--accent-2); }
  .tree-row .badge[data-k="bug"] { background: rgba(251,113,133,0.18); color: #fb7185; }
  .tree-row .badge[data-k="critique"] { background: rgba(245,158,11,0.18); color: #f59e0b; }
  .tree-row .recent { width: 5px; height: 5px; border-radius: 50%; background: var(--accent); flex-shrink: 0; }
  .tree-children { padding-left: 16px; border-left: 1px dashed var(--border-soft); margin-left: 24px; }
</style>
