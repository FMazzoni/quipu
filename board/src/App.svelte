<script lang="ts">
  import { onMount } from 'svelte';
  import { data } from './stores/ui';
  import { loadData } from './lib/data';
  import StateChips from './components/StateChips.svelte';
  import TaskTree from './components/TaskTree.svelte';
  import DetailPane from './components/DetailPane.svelte';
  import ForceGraph from './components/ForceGraph.svelte';
  import Modal from './components/Modal.svelte';

  let loading = $state(true);

  onMount(async () => {
    const payload = await loadData();
    data.set(payload);
    loading = false;
  });
</script>

<header>
  <div class="brand">quipu <span class="dim">/ board</span></div>
  <StateChips />
  <div class="spacer"></div>
  <div class="meta">{loading ? 'loading…' : 'ready'}</div>
</header>

<main>
  <aside class="rail-left">
    <TaskTree />
  </aside>
  <section class="detail">
    <DetailPane />
  </section>
  <aside class="rail-right">
    <ForceGraph />
  </aside>
</main>

<Modal />

<style>
  header {
    background: var(--surface);
    border-bottom: 1px solid var(--border);
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 0 20px;
    z-index: 10;
  }
  .brand { font-weight: 700; font-size: 15px; letter-spacing: -0.3px; }
  .brand .dim { color: var(--muted); font-weight: 400; }
  .spacer { flex: 1; }
  .meta { color: var(--muted); font-size: 12px; }
  main {
    display: grid;
    grid-template-columns: 320px 1fr 360px;
    overflow: hidden;
  }
  aside, section { overflow: hidden; display: flex; flex-direction: column; }
  .rail-left { background: var(--surface); border-right: 1px solid var(--border); }
  .rail-right { background: var(--surface); border-left: 1px solid var(--border); }
  .detail { padding: 0; overflow: auto; }
</style>
