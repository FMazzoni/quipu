// UI state stores. Plain Svelte `writable`s; components subscribe via `$store`.
// (Svelte 5 runes are great inside components but stores are still the
// cleanest way to share reactive state across modules.)

import { writable, derived, type Readable } from 'svelte/store';
import type { Payload, Task, State } from '../lib/data';
import { EMPTY } from '../lib/data';

export const data = writable<Payload>(EMPTY);
export const selected = writable<string | null>(null);
export const stateFilter = writable<Set<State>>(
  new Set<State>(['pending', 'ready', 'assigned', 'running']),
);
export const tagFilter = writable<Set<string>>(new Set<string>());
export const search = writable<string>('');
export const expanded = writable<Set<string>>(new Set<string>());

export function toggleState(s: State): void {
  stateFilter.update((set) => {
    const next = new Set(set);
    if (next.has(s)) next.delete(s);
    else next.add(s);
    return next;
  });
}

export function toggleTag(tag: string): void {
  tagFilter.update((set) => {
    const next = new Set(set);
    if (next.has(tag)) next.delete(tag);
    else next.add(tag);
    return next;
  });
}

export function toggleExpanded(id: string): void {
  expanded.update((set) => {
    const next = new Set(set);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    return next;
  });
}

function matchesFilter(
  t: Task,
  states: Set<State>,
  tags: Set<string>,
  q: string,
): boolean {
  if (!states.has(t.state)) return false;
  if (tags.size > 0) {
    const have = new Set(t.tags || []);
    for (const want of tags) if (!have.has(want)) return false;
  }
  if (q) {
    const lq = q.toLowerCase();
    const hay = `${t.title || ''} ${t.description || ''} ${t.display_id}`.toLowerCase();
    if (!hay.includes(lq)) return false;
  }
  return true;
}

export const filteredTasks: Readable<Task[]> = derived(
  [data, stateFilter, tagFilter, search],
  ([$data, $sf, $tf, $q]) => $data.tasks.filter((t) => matchesFilter(t, $sf, $tf, $q)),
);

// Derived indexes for tree + graph rendering.
export interface Indexes {
  byId: Map<string, Task>;
  childrenOf: Map<string, string[]>; // task -> tasks it depends on (visually below)
  parentsOf: Map<string, string[]>;  // task -> tasks that depend on it (visually above)
  eventsByTask: Map<string, any[]>;
}

export const indexes: Readable<Indexes> = derived(data, ($data) => {
  const byId = new Map<string, Task>();
  const childrenOf = new Map<string, string[]>();
  const parentsOf = new Map<string, string[]>();
  const eventsByTask = new Map<string, any[]>();
  for (const t of $data.tasks) {
    byId.set(t.display_id, t);
    childrenOf.set(t.display_id, []);
    parentsOf.set(t.display_id, []);
  }
  for (const d of $data.deps) {
    childrenOf.get(d.from)?.push(d.to);
    parentsOf.get(d.to)?.push(d.from);
  }
  for (const ev of $data.events) {
    if (!ev.task) continue;
    let arr = eventsByTask.get(ev.task);
    if (!arr) {
      arr = [];
      eventsByTask.set(ev.task, arr);
    }
    arr.push(ev);
  }
  return { byId, childrenOf, parentsOf, eventsByTask };
});

export const rootTasks: Readable<Task[]> = derived(
  [data, indexes],
  ([$data, $idx]) => {
    return $data.tasks
      .filter((t) => ($idx.parentsOf.get(t.display_id) || []).length === 0)
      .slice()
      .sort((a, b) => b.id - a.id);
  },
);
