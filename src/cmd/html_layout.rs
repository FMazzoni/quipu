//! Minimal layered DAG layout for the `qp html` dashboard.
//!
//! Pure Rust, no external dependencies. Longest-path layering:
//!   layer(node) = 1 + max(layer(dep) for dep in deps(node))
//! Within a layer, nodes are sorted by id and spaced evenly across the
//! layer's width. Coordinates are returned in an abstract grid (column,
//! layer) that the renderer scales to SVG pixels.
//!
//! Edges are returned in the direction `task -> dep` (dependency edge);
//! the caller draws them however it likes.

use std::collections::{BTreeMap, HashMap, HashSet};

/// One node in the layout.
pub struct LaidNode {
    pub id: i64,
    pub display_id: String,
    pub state: String,
    #[allow(dead_code)]
    pub title: String,
    pub layer: usize,
    /// 0-based column within the layer.
    pub col: usize,
    /// Total nodes in this layer (so the renderer can compute x = (col+0.5)/cols * width).
    pub layer_width: usize,
}

pub struct Edge {
    pub from: i64, // task
    pub to: i64,   // depends_on
}

pub struct Layout {
    pub nodes: Vec<LaidNode>,
    pub edges: Vec<Edge>,
    pub layer_count: usize,
}

/// Inputs:
///   - `tasks`: (id, display_id, state, title)
///   - `deps`:  (task_id, depends_on_task_id) — edge task -> dep
///
/// Layering: nodes with no out-going dep edges (i.e. nodes that nothing else
/// depends on) sit on layer 0 at the top — these are the waves / roots.
/// Leaves (no in-coming dep, i.e. nothing they depend on) drift to the bottom.
/// We achieve this by:
///   layer(n) = 1 + max(layer(d) for d in deps(n)) [d depends on n via reverse]
/// We invert: for each edge task→dep, parent=task is "above" dep. So we walk
/// from roots (no incoming dep-of edges) downward.
pub fn layout(
    tasks: &[(i64, String, String, String)],
    deps: &[(i64, i64)],
) -> Layout {
    let task_ids: HashSet<i64> = tasks.iter().map(|t| t.0).collect();

    // Build adjacency: for each node, list of nodes that depend on it (children below).
    // edge (task → dep) means "task depends on dep". We want roots = tasks nothing depends
    // on. So compute in-degree on the *reverse* graph: in_count[node] = number of tasks
    // that node depends on... no.  Simpler: roots = tasks with no incoming edges in the
    // task→dep graph, i.e. no edge points TO them. Those tasks are not depended upon —
    // they are the top of the wave (e.g. a coordinator ticket whose deps are leaves).
    let mut in_count: HashMap<i64, usize> = task_ids.iter().map(|i| (*i, 0)).collect();
    let mut children: HashMap<i64, Vec<i64>> = task_ids.iter().map(|i| (*i, Vec::new())).collect();
    for (from, to) in deps {
        if !task_ids.contains(from) || !task_ids.contains(to) { continue; }
        // edge from → to: `to` has an incoming edge.
        *in_count.entry(*to).or_insert(0) += 1;
        children.entry(*from).or_default().push(*to);
    }

    // Topological BFS from roots. Layer = longest path from any root.
    let mut layer: HashMap<i64, usize> = HashMap::new();
    let mut queue: Vec<i64> = in_count.iter()
        .filter(|(_, c)| **c == 0)
        .map(|(id, _)| *id)
        .collect();
    queue.sort();
    for id in &queue { layer.insert(*id, 0); }

    // Kahn's algorithm — process in deterministic order, refining layer to max.
    let mut remaining: HashMap<i64, usize> = in_count.clone();
    let mut head = 0;
    while head < queue.len() {
        let n = queue[head]; head += 1;
        let nlayer = *layer.get(&n).unwrap_or(&0);
        let mut kids = children.get(&n).cloned().unwrap_or_default();
        kids.sort();
        for c in kids {
            // Promote child's layer.
            let new_layer = nlayer + 1;
            let cur = layer.get(&c).copied().unwrap_or(0);
            if new_layer > cur { layer.insert(c, new_layer); }
            // Decrement remaining incoming; enqueue when zero.
            let entry = remaining.entry(c).or_insert(0);
            if *entry > 0 { *entry -= 1; }
            if *entry == 0 && !queue.contains(&c) {
                queue.push(c);
            }
        }
    }

    // Any leftover (cycle) — assign layer 0.
    for id in &task_ids {
        layer.entry(*id).or_insert(0);
    }

    // Group by layer, sort within layer by id.
    let mut by_layer: BTreeMap<usize, Vec<i64>> = BTreeMap::new();
    for (id, l) in &layer {
        by_layer.entry(*l).or_default().push(*id);
    }
    for v in by_layer.values_mut() { v.sort(); }

    let layer_count = by_layer.keys().last().map(|k| k + 1).unwrap_or(0);

    // Build LaidNode list, indexed by task lookup.
    let task_lookup: HashMap<i64, (&String, &String, &String)> =
        tasks.iter().map(|t| (t.0, (&t.1, &t.2, &t.3))).collect();

    let mut nodes: Vec<LaidNode> = Vec::with_capacity(tasks.len());
    for (l, ids) in &by_layer {
        let width = ids.len();
        for (col, id) in ids.iter().enumerate() {
            if let Some((did, st, title)) = task_lookup.get(id) {
                nodes.push(LaidNode {
                    id: *id,
                    display_id: (*did).clone(),
                    state: (*st).clone(),
                    title: (*title).clone(),
                    layer: *l,
                    col,
                    layer_width: width,
                });
            }
        }
    }

    let edges: Vec<Edge> = deps.iter()
        .filter(|(f, t)| task_ids.contains(f) && task_ids.contains(t))
        .map(|(f, t)| Edge { from: *f, to: *t })
        .collect();

    Layout { nodes, edges, layer_count }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_chain() {
        // QP-1 depends on QP-2 depends on QP-3
        let tasks = vec![
            (1, "QP-1".into(), "ready".into(), "a".into()),
            (2, "QP-2".into(), "ready".into(), "b".into()),
            (3, "QP-3".into(), "ready".into(), "c".into()),
        ];
        let deps = vec![(1, 2), (2, 3)];
        let l = layout(&tasks, &deps);
        assert_eq!(l.layer_count, 3);
        let layer_of = |id: i64| l.nodes.iter().find(|n| n.id == id).unwrap().layer;
        assert_eq!(layer_of(1), 0);
        assert_eq!(layer_of(2), 1);
        assert_eq!(layer_of(3), 2);
    }

    #[test]
    fn diamond() {
        // 1 -> {2,3} -> 4
        let tasks = vec![
            (1, "QP-1".into(), "ready".into(), "a".into()),
            (2, "QP-2".into(), "ready".into(), "b".into()),
            (3, "QP-3".into(), "ready".into(), "c".into()),
            (4, "QP-4".into(), "ready".into(), "d".into()),
        ];
        let deps = vec![(1, 2), (1, 3), (2, 4), (3, 4)];
        let l = layout(&tasks, &deps);
        let layer_of = |id: i64| l.nodes.iter().find(|n| n.id == id).unwrap().layer;
        assert_eq!(layer_of(1), 0);
        assert_eq!(layer_of(2), 1);
        assert_eq!(layer_of(3), 1);
        assert_eq!(layer_of(4), 2);
    }

    #[test]
    fn isolated_nodes() {
        let tasks = vec![
            (1, "QP-1".into(), "ready".into(), "a".into()),
            (2, "QP-2".into(), "ready".into(), "b".into()),
        ];
        let l = layout(&tasks, &[]);
        assert_eq!(l.layer_count, 1);
        assert_eq!(l.nodes.len(), 2);
    }
}
