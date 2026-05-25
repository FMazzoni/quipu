// Shared d3-force rendering used by both the mini rail graph and the modal.
// Pulled out of the component so both call-sites share the exact same
// drawing logic (ported verbatim from the v2.3 mockup).

import * as d3 from 'd3';
import type { Task, Dep } from './data';
import { STATE_COLOR } from './data';
import type { Indexes } from '../stores/ui';

export interface GraphNode {
  id: string;
  state: string;
  title: string;
  r: number;
  x?: number; y?: number; fx?: number | null; fy?: number | null;
}
export interface GraphLink {
  source: GraphNode | string;
  target: GraphNode | string;
}

export interface DrawOpts {
  linkDistance?: number;
  charge?: number;
  sticky?: boolean;
  selected?: string | null;
  onSelect?: (id: string) => void;
}

export function buildGraphData(
  taskIds: string[],
  indexes: Indexes,
  deps: Dep[],
): { nodes: GraphNode[]; links: GraphLink[] } {
  const idSet = new Set(taskIds);
  const nodes: GraphNode[] = taskIds.map((id) => {
    const t = indexes.byId.get(id)!;
    const inDeg = (indexes.parentsOf.get(id) || []).length;
    const outDeg = (indexes.childrenOf.get(id) || []).length;
    return {
      id,
      state: t.state,
      title: t.title,
      r: 6 + Math.min(8, (inDeg + outDeg) * 1.5),
    };
  });
  const links: GraphLink[] = [];
  for (const d of deps) {
    if (idSet.has(d.from) && idSet.has(d.to)) {
      links.push({ source: d.from, target: d.to });
    }
  }
  return { nodes, links };
}

export interface DrawHandle {
  destroy: () => void;
  reset: () => void;
}

export function drawGraph(
  svgEl: SVGSVGElement,
  nodes: GraphNode[],
  links: GraphLink[],
  opts: DrawOpts = {},
): DrawHandle {
  while (svgEl.firstChild) svgEl.removeChild(svgEl.firstChild);
  const svg = d3.select(svgEl);
  const W = svgEl.clientWidth || 360;
  const H = svgEl.clientHeight || 400;
  const g = svg.append('g');

  svg.append('defs').append('marker')
    .attr('id', `arr-${Math.random().toString(36).slice(2, 8)}`)
    .attr('viewBox', '0 -5 10 10').attr('refX', 14).attr('refY', 0)
    .attr('markerWidth', 6).attr('markerHeight', 6).attr('orient', 'auto')
    .append('path').attr('d', 'M0,-4L8,0L0,4').attr('fill', 'var(--muted-2)');
  const markerId = svg.select('defs marker').attr('id');

  const sim = d3.forceSimulation<GraphNode>(nodes)
    .force('link', d3.forceLink<GraphNode, GraphLink>(links).id((d) => d.id)
      .distance(opts.linkDistance ?? 60).strength(0.6))
    .force('charge', d3.forceManyBody().strength(opts.charge ?? -180))
    .force('center', d3.forceCenter(W / 2, H / 2))
    .force('collide', d3.forceCollide<GraphNode>().radius((d) => d.r + 4));

  const link = g.append('g').selectAll<SVGLineElement, GraphLink>('line')
    .data(links).enter().append('line')
    .attr('class', 'link').attr('marker-end', `url(#${markerId})`);

  const node = g.append('g').selectAll<SVGGElement, GraphNode>('g')
    .data(nodes).enter().append('g')
    .attr('class', 'node-g').style('cursor', 'pointer');

  node.append('circle')
    .attr('class', (d) => 'node-circle' + (d.id === opts.selected ? ' selected' : ''))
    .attr('r', (d) => d.r)
    .attr('fill', (d) => (STATE_COLOR as any)[d.state] + '30')
    .attr('stroke', (d) => (STATE_COLOR as any)[d.state]);

  node.append('text')
    .attr('class', (d) => 'node-label' + (d.id === opts.selected ? ' large' : ''))
    .attr('dy', (d) => d.r + 12)
    .attr('text-anchor', 'middle')
    .text((d) => d.id);

  node.call(d3.drag<SVGGElement, GraphNode>()
    .on('start', (event, d) => { if (!event.active) sim.alphaTarget(0.3).restart(); d.fx = d.x; d.fy = d.y; })
    .on('drag', (event, d) => { d.fx = event.x; d.fy = event.y; })
    .on('end', (event, d) => {
      if (!event.active) sim.alphaTarget(0);
      if (!opts.sticky) { d.fx = null; d.fy = null; }
    }));

  const neighborSet = (id: string) => {
    const s = new Set<string>([id]);
    for (const l of links) {
      const src = (l.source as GraphNode).id ?? (l.source as string);
      const tgt = (l.target as GraphNode).id ?? (l.target as string);
      if (src === id) s.add(tgt);
      if (tgt === id) s.add(src);
    }
    return s;
  };
  node.on('mouseenter', (_event, d) => {
    const nbs = neighborSet(d.id);
    node.classed('dim', (n) => !nbs.has(n.id));
    link.classed('dim', (l) => {
      const src = (l.source as GraphNode).id; const tgt = (l.target as GraphNode).id;
      return src !== d.id && tgt !== d.id;
    }).classed('highlighted', (l) => {
      const src = (l.source as GraphNode).id; const tgt = (l.target as GraphNode).id;
      return src === d.id || tgt === d.id;
    });
  }).on('mouseleave', () => {
    node.classed('dim', false);
    link.classed('dim', false).classed('highlighted', false);
  });

  node.on('click', (event, d) => {
    event.stopPropagation();
    opts.onSelect?.(d.id);
  });

  const zoom = d3.zoom<SVGSVGElement, unknown>()
    .scaleExtent([0.3, 4])
    .on('zoom', (ev) => g.attr('transform', ev.transform.toString()));
  svg.call(zoom);

  sim.on('tick', () => {
    link
      .attr('x1', (d) => (d.source as GraphNode).x ?? 0)
      .attr('y1', (d) => (d.source as GraphNode).y ?? 0)
      .attr('x2', (d) => (d.target as GraphNode).x ?? 0)
      .attr('y2', (d) => (d.target as GraphNode).y ?? 0);
    node.attr('transform', (d) => `translate(${d.x},${d.y})`);
  });

  return {
    destroy: () => { sim.stop(); svg.selectAll('*').remove(); },
    reset: () => { svg.transition().duration(300).call(zoom.transform, d3.zoomIdentity); },
  };
}
