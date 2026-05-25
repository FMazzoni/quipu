<script lang="ts">
  import { selected, indexes, toggleTag } from '../stores/ui';
  import type { Task } from '../lib/data';
  import { classifyTags } from '../lib/classify';

  let task = $derived<Task | undefined>(
    $selected ? $indexes.byId.get($selected) : undefined
  );

  let events = $derived.by(() => {
    if (!task) return [];
    return ($indexes.eventsByTask.get(task.display_id) || []).slice().reverse();
  });

  let frictionEvents = $derived(
    events.filter((e: any) => e.kind === 'decision' && e.payload?.auto)
  );
  let lifecycleEvents = $derived(
    events.filter((e: any) =>
      ['state_change', 'dep_added', 'dep_removed', 'edit', 'blocker'].includes(e.kind),
    )
  );

  let parents = $derived(task ? ($indexes.parentsOf.get(task.display_id) || []) : []);
  let children = $derived(task ? ($indexes.childrenOf.get(task.display_id) || []) : []);
  let classified = $derived(task ? classifyTags(task.tags) : null);

  function eventBody(e: any): string {
    if (e.kind === 'state_change') return '→ ' + (e.payload?.to || '?');
    if (e.kind === 'dep_added' || e.kind === 'dep_removed') return e.payload?.on || '';
    if (e.kind === 'edit') return Object.keys(e.payload?.changes || {}).join(', ');
    if (e.kind === 'blocker') return e.payload?.title || '';
    try { return JSON.stringify(e.payload).slice(0, 80); } catch { return ''; }
  }

  function selectRelated(id: string) { selected.set(id); }
</script>

<div class="detail-inner">
  {#if !task}
    <div class="detail-empty">
      <div class="icon">◇</div>
      <h2>nothing selected</h2>
      <p>Pick a ticket from the left rail. Default filter shows in-flight work; toggle state chips above to see what's shipped.</p>
    </div>
  {:else}
    <div class="detail-id-state-row">
      <span class="detail-id">{task.display_id}</span>
      <span class="detail-state-pill" data-s={task.state}>{task.state}</span>
      {#if classified?.kind}
        <span class="kind-badge" data-k={classified.kind}>{classified.kind}</span>
      {/if}
      {#if task.tier}
        <span class="kind-badge">{task.tier}</span>
      {/if}
    </div>
    <h1 class="detail-title">{task.title}</h1>

    <div class="meta-rows">
      <div class="k">agent</div>
      <div class="v">
        {#if task.agent}
          <span class="agent-badge"><span class="icon">◉</span>{task.agent}</span>
        {:else}
          <span class="empty">—</span>
        {/if}
      </div>

      {#if classified?.commit}
        <div class="k">commit</div>
        <div class="v">
          <a class="commit-link" href="#" onclick={(e) => { e.preventDefault(); }}>
            <span>⌘</span>{classified.commit}
          </a>
        </div>
      {/if}

      {#if classified?.plan}
        <div class="k">plan</div>
        <div class="v">
          <a class="plan-link" href="#" onclick={(e) => { e.preventDefault(); }}>
            <span>◫</span>{classified.plan}
          </a>
        </div>
      {/if}

      {#if classified?.critique}
        <div class="k">critique</div>
        <div class="v">
          <a class="critique-link" href="#" onclick={(e) => { e.preventDefault(); }}>
            <span>◔</span>{classified.critique}
          </a>
        </div>
      {/if}

      {#if classified?.harness}
        <div class="k">harness</div>
        <div class="v"><span class="harness-badge">{classified.harness}</span></div>
      {/if}

      {#if classified && classified.generic.length > 0}
        <div class="k">tags</div>
        <div class="v">
          <div class="tag-list">
            {#each classified.generic as g}
              <span class="tag" onclick={() => toggleTag(g)} role="button" tabindex="0">{g}</span>
            {/each}
          </div>
        </div>
      {/if}
    </div>

    <div class="section">
      <div class="section-title">Description</div>
      {#if task.description}
        <div class="description">{task.description}</div>
      {:else}
        <div class="description empty">No description.</div>
      {/if}
    </div>

    {#if parents.length + children.length > 0}
      <div class="section">
        <div class="section-title">Related <span class="count">{parents.length + children.length}</span></div>
        <div class="related-list">
          {#each parents as pid (pid)}
            {@const p = $indexes.byId.get(pid)}
            {#if p}
              <div class="related-row" data-state={p.state} onclick={() => selectRelated(pid)} role="button" tabindex="0">
                <span class="arrow">part of</span>
                <span class="state-dot"></span>
                <span class="id">{pid}</span>
                <span class="title">{p.title}</span>
              </div>
            {/if}
          {/each}
          {#each children as cid (cid)}
            {@const c = $indexes.byId.get(cid)}
            {#if c}
              <div class="related-row" data-state={c.state} onclick={() => selectRelated(cid)} role="button" tabindex="0">
                <span class="arrow">blocks on</span>
                <span class="state-dot"></span>
                <span class="id">{cid}</span>
                <span class="title">{c.title}</span>
              </div>
            {/if}
          {/each}
        </div>
      </div>
    {/if}

    {#if frictionEvents.length > 0}
      <div class="section">
        <div class="section-title">Friction notes <span class="count">{frictionEvents.length}</span></div>
        {#each frictionEvents as e}
          <div class="friction-card">
            <div class="header">{e.ts.slice(0, 16).replace('T', ' ')} · {e.agent_id || '—'}</div>
            <div class="text">{e.payload?.text || ''}</div>
          </div>
        {/each}
      </div>
    {/if}

    {#if lifecycleEvents.length > 0}
      <div class="section">
        <div class="section-title">Lifecycle <span class="count">{lifecycleEvents.length}</span></div>
        <div class="event-list">
          {#each lifecycleEvents.slice(0, 20) as e}
            <div class="event" data-k={e.kind}>
              <div class="ts">{e.ts.slice(11, 19)}</div>
              <div class="kind">{e.kind}</div>
              <div class="body">{eventBody(e)}</div>
            </div>
          {/each}
        </div>
      </div>
    {/if}
  {/if}
</div>

<style>
  .detail-inner { padding: 28px 32px; max-width: 760px; }
  .detail-empty { color: var(--muted); text-align: center; padding: 100px 20px; }
  .detail-empty .icon { font-size: 28px; opacity: 0.4; margin-bottom: 16px; }
  .detail-empty h2 { font-size: 15px; margin-bottom: 6px; color: var(--text); font-weight: 500; }
  .detail-empty p { font-size: 13px; line-height: 1.6; max-width: 320px; margin: 0 auto; }

  .detail-id-state-row { display: flex; align-items: center; gap: 10px; margin-bottom: 10px; font-size: 12px; }
  .detail-id { font-family: ui-monospace, monospace; color: var(--muted); font-size: 13px; }
  .detail-state-pill { padding: 3px 10px; border-radius: 4px; font-size: 11px; text-transform: uppercase; letter-spacing: 0.6px; font-weight: 600; }
  .detail-state-pill[data-s="ready"]    { background: rgba(109,217,123,0.15); color: var(--s-ready); }
  .detail-state-pill[data-s="pending"]  { background: rgba(251,191,36,0.15);  color: var(--s-pending); }
  .detail-state-pill[data-s="assigned"] { background: rgba(77,206,240,0.15);  color: var(--s-assigned); }
  .detail-state-pill[data-s="running"]  { background: rgba(122,178,255,0.15); color: var(--s-running); }
  .detail-state-pill[data-s="done"]     { background: rgba(107,112,128,0.18); color: var(--s-done); }
  .detail-state-pill[data-s="cancelled"] { background: rgba(74,79,96,0.15);    color: var(--s-cancelled); }
  .kind-badge {
    padding: 3px 8px; border-radius: 3px; font-size: 10px; text-transform: uppercase; letter-spacing: 0.6px;
    background: var(--surface-3); color: var(--muted); font-weight: 600;
  }
  .kind-badge[data-k="wave"]    { background: rgba(167,139,250,0.15); color: var(--accent-2); }
  .kind-badge[data-k="bug"]     { background: rgba(251,113,133,0.15); color: #fb7185; }
  .kind-badge[data-k="impl"]    { background: rgba(74,222,128,0.15); color: #4ade80; }
  .kind-badge[data-k="critique"] { background: rgba(245,158,11,0.15); color: #f59e0b; }
  .kind-badge[data-k="feature"] { background: rgba(56,189,248,0.15); color: #38bdf8; }
  .detail-title { font-size: 22px; font-weight: 600; letter-spacing: -0.3px; line-height: 1.3; margin-bottom: 4px; }

  .meta-rows { display: grid; grid-template-columns: 88px 1fr; gap: 4px 18px; margin: 20px 0; font-size: 13px; }
  .meta-rows .k { color: var(--muted); font-size: 11px; text-transform: uppercase; letter-spacing: 0.6px; padding-top: 4px; }
  .meta-rows .v { color: var(--text); display: flex; align-items: center; gap: 6px; min-width: 0; flex-wrap: wrap; }
  .meta-rows .v .empty { color: var(--muted); font-style: italic; }
  .commit-link {
    display: inline-flex; align-items: center; gap: 6px; padding: 3px 9px;
    background: rgba(167,139,250,0.1); border: 1px solid rgba(167,139,250,0.2); border-radius: 4px;
    color: var(--accent-2); font-family: ui-monospace, monospace; font-size: 12px;
  }
  .commit-link:hover { background: rgba(167,139,250,0.18); text-decoration: none; }
  .plan-link, .critique-link {
    display: inline-flex; align-items: center; gap: 6px; padding: 3px 9px;
    background: var(--surface-2); border: 1px solid var(--border); border-radius: 4px;
    color: var(--accent); font-size: 12px;
  }
  .plan-link:hover, .critique-link:hover { background: var(--surface-3); text-decoration: none; }
  .agent-badge {
    display: inline-flex; align-items: center; gap: 6px; padding: 3px 9px; background: var(--surface-2);
    border-radius: 4px; font-family: ui-monospace, monospace; font-size: 12px; color: var(--text);
  }
  .agent-badge .icon { color: var(--muted); }
  .harness-badge {
    padding: 2px 7px; border-radius: 3px; background: rgba(251,146,60,0.12); color: #fb923c;
    font-size: 11px; font-family: ui-monospace, monospace;
  }

  .section { margin: 28px 0 0; }
  .section-title {
    font-size: 10px; text-transform: uppercase; letter-spacing: 0.8px; color: var(--muted); font-weight: 600;
    margin-bottom: 12px; display: flex; align-items: center; gap: 8px;
  }
  .section-title .count { padding: 1px 6px; border-radius: 8px; background: var(--surface-2); color: var(--muted-2); font-size: 10px; font-weight: 500; }

  .description {
    background: var(--surface); border: 1px solid var(--border-soft); border-left: 2px solid var(--accent);
    padding: 14px 18px; border-radius: 0 4px 4px 0; color: var(--text); font-size: 13.5px; line-height: 1.7; white-space: pre-wrap;
  }
  .description.empty { color: var(--muted); font-style: italic; padding: 4px 0; background: none; border: none; }
  .tag-list { display: flex; flex-wrap: wrap; gap: 5px; }
  .tag {
    padding: 2px 8px; border-radius: 10px; background: var(--surface-2); color: var(--muted);
    font-size: 11px; font-family: ui-monospace, monospace; border: 1px solid transparent;
    cursor: pointer;
  }
  .tag:hover { background: var(--surface-3); color: var(--text); }

  .event-list { display: flex; flex-direction: column; }
  .event { display: grid; grid-template-columns: 60px 90px 1fr; gap: 12px; padding: 7px 0; border-bottom: 1px solid var(--border-soft); font-size: 13px; align-items: start; }
  .event:last-child { border-bottom: none; }
  .event .ts { color: var(--muted); font-family: ui-monospace, monospace; font-size: 11px; padding-top: 1px; }
  .event .kind { font-family: ui-monospace, monospace; font-size: 10px; padding: 2px 7px; border-radius: 3px; background: var(--surface-2); color: var(--muted); width: max-content; }
  .event[data-k="state_change"] .kind { color: var(--s-running); }
  .event[data-k="decision"] .kind { color: var(--accent); }
  .event[data-k="dep_added"] .kind, .event[data-k="dep_removed"] .kind { color: #fb923c; }
  .event .body { color: var(--text); word-break: break-word; }

  .friction-card {
    background: linear-gradient(180deg, rgba(122,178,255,0.05), transparent);
    border: 1px solid rgba(122,178,255,0.15); border-radius: 6px; padding: 12px 14px; margin-bottom: 8px; position: relative;
  }
  .friction-card::before { content: "“"; position: absolute; top: -2px; right: 10px; font-size: 32px; color: rgba(122,178,255,0.2); font-family: Georgia, serif; }
  .friction-card .header { font-size: 11px; color: var(--muted); margin-bottom: 6px; font-family: ui-monospace, monospace; }
  .friction-card .text { color: var(--text); font-size: 13px; line-height: 1.6; }

  .related-list { display: flex; flex-direction: column; gap: 4px; }
  .related-row { padding: 8px 12px; border-radius: 4px; background: var(--surface); border: 1px solid var(--border-soft); display: flex; gap: 10px; align-items: center; font-size: 13px; transition: background 0.1s; cursor: pointer; }
  .related-row:hover { background: var(--surface-2); }
  .related-row .arrow { color: var(--muted-2); font-size: 11px; min-width: 64px; text-transform: uppercase; letter-spacing: 0.5px; }
  .related-row .id { font-family: ui-monospace, monospace; color: var(--accent); min-width: 48px; font-size: 12px; }
  .related-row .title { color: var(--text); flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .related-row .state-dot { width: 5px; height: 5px; border-radius: 50%; }
  .related-row[data-state="ready"] .state-dot { background: var(--s-ready); }
  .related-row[data-state="done"] .state-dot { background: var(--s-done); }
  .related-row[data-state="running"] .state-dot { background: var(--s-running); }
  .related-row[data-state="pending"] .state-dot { background: var(--s-pending); }
  .related-row[data-state="assigned"] .state-dot { background: var(--s-assigned); }
</style>
