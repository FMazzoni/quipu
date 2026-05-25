# quipu-board

Static dashboard for a quipu store. Bun + Svelte 5 + Vite. Reads a JSON dump
of the store state (the same shape `qp report` will emit) and renders it.

## Quick start

```sh
bun install
bun run dev          # http://localhost:5173 with HMR
```

## Producing real data

The dashboard reads `./public/data.json` at runtime. To dump your current
store state into it:

```sh
qp report --json > board/public/data.json   # path may vary while qp report --json is in flight
```

If `data.json` is missing the page falls back to an empty payload — useful
when developing the layout without a populated store.

## Building a static bundle

```sh
bun run build        # outputs to ./dist/
```

`./dist/index.html` (and its sibling assets) is fully static — open it directly
in a browser, or serve it with any static-file host.

## Layout

```
src/
  App.svelte               top-level header + 3-col layout + modal mount
  main.ts                  Svelte 5 mount point
  app.css                  global tokens (CSS vars for the v2.3 palette)
  components/
    StateChips.svelte      header state filter row
    TaskTree.svelte        left rail — dep-hierarchy tree + tag facets
    DetailPane.svelte      centre — selected ticket detail
    ForceGraph.svelte      right rail — d3-force neighborhood graph
    Modal.svelte           full-graph overlay
  stores/
    ui.ts                  selected / stateFilter / tagFilter / search / derived
  lib/
    data.ts                types + loadData() for ./data.json
    classify.ts            typed-tag classifier (commit/plan/critique/...)
```
