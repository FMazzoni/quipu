//! Render the dependency DAG.
//!
#![doc = include_str!("../../docs/modules/tree.md")]

use crate::{db, id, store};
use anyhow::Result;
use clap::Args;
use std::collections::{HashMap, HashSet};

#[derive(Args, Debug)]
pub struct TreeArgs {
    /// Optional task id — when present, restrict output to this task + its transitive deps.
    pub task: Option<String>,
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub tier: Option<String>,
    #[arg(long)]
    pub show_tags: bool,
    /// Print each task's description on indented continuation lines.
    #[arg(long = "with-description")]
    pub with_description: bool,
}

pub fn run(db_path: &std::path::Path, a: TreeArgs) -> Result<()> {
    let conn = db::open(db_path)?;

    let subtree: Option<HashSet<i64>> = if let Some(t) = &a.task {
        let root = id::resolve(&conn, t)?;
        Some(store::subtree_ids(&conn, root)?)
    } else {
        None
    };

    let filter = store::TaskFilter {
        state: None,
        assigned_to_glob: None,
        tag_globs: &[],
        tier: a.tier.as_deref(),
    };
    let mut tasks: Vec<store::TaskRow> = Vec::new();
    for row in store::tasks(&conn, &filter)? {
        if let Some(set) = &subtree {
            if !set.contains(&row.id) {
                continue;
            }
        }
        tasks.push(row);
    }

    // Split by mode at the source. `blocks` becomes the `<- [...]` annotation;
    // `contains` becomes the nesting.
    let mut blocks: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut contains: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut container_of: HashMap<i64, Vec<i64>> = HashMap::new();
    // Blocking edges are filtered to *unresolved* blockers, matching what
    // `blocked_by` means everywhere else. Rendering resolved ones was the old
    // behaviour and was survivable while the annotation was an untyped
    // `<- [...]`; it stopped being survivable once `--json` started calling the
    // field `blocked_by`, because `show` and `list` filter and this did not —
    // one name, two meanings. Containment is deliberately not filtered: a
    // container's manifest includes the parts already finished.
    let mut s = conn.prepare(
        "SELECT d.task_id, d.depends_on_task_id, d.mode
           FROM dep d JOIN task t ON t.id = d.depends_on_task_id
          WHERE d.mode = 'contains' OR t.state NOT IN ('done','cancelled')
          ORDER BY d.depends_on_task_id",
    )?;
    for r in s.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
        ))
    })? {
        let (t, d, mode) = r?;
        if mode == db::MODE_CONTAINS {
            contains.entry(t).or_default().push(d);
            container_of.entry(d).or_default().push(t);
        } else {
            blocks.entry(t).or_default().push(d);
        }
    }

    // id -> display_id lookup for rendering dep refs in display-id format.
    let mut display_by_id: HashMap<i64, String> = HashMap::new();
    let mut s = conn.prepare("SELECT id, display_id FROM task")?;
    for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (i, d) = r?;
        display_by_id.insert(i, d);
    }

    let mut tags_by: HashMap<i64, Vec<String>> = HashMap::new();
    if a.show_tags || a.json {
        let mut s = conn.prepare("SELECT task_id, name FROM tag")?;
        for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (t, n) = r?;
            tags_by.entry(t).or_default().push(n);
        }
    }

    if a.json {
        let mut out = Vec::new();
        for row in &tasks {
            let mut obj = serde_json::json!({
                "id": row.id, "display_id": row.display_id, "title": row.title,
                "state": row.state, "tier": row.tier,
                // `depends_on` keeps its original meaning — every dep edge,
                // both modes — so an existing consumer is unaffected. The two
                // split lists are additive and carry display ids, matching the
                // rest of the JSON surface.
                "depends_on": blocks.get(&row.id).iter().copied().flatten()
                    .chain(contains.get(&row.id).iter().copied().flatten())
                    .copied().collect::<Vec<i64>>(),
                "blocked_by": dids(blocks.get(&row.id), &display_by_id),
                "contains": dids(contains.get(&row.id), &display_by_id),
                "part_of": dids(container_of.get(&row.id), &display_by_id),
                "tags": tags_by.get(&row.id).cloned().unwrap_or_default(),
            });
            if let Some(d) = &row.description {
                obj.as_object_mut()
                    .unwrap()
                    .insert("description".into(), serde_json::Value::String(d.clone()));
            }
            out.push(obj);
        }
        println!("{}", serde_json::to_string(&out)?);
    } else {
        let by_id: HashMap<i64, &store::TaskRow> = tasks.iter().map(|r| (r.id, r)).collect();
        // Roots are everything with no container *inside the rendered set*: a
        // slice whose wave was filtered out by `--tier` must still print, at
        // the top level, rather than vanish with its parent.
        let roots: Vec<&store::TaskRow> = tasks
            .iter()
            .filter(|r| {
                !container_of
                    .get(&r.id)
                    .is_some_and(|ps| ps.iter().any(|p| by_id.contains_key(p)))
            })
            .collect();
        let mut seen: HashSet<i64> = HashSet::new();
        for root in roots {
            print_node(
                root,
                0,
                &by_id,
                &contains,
                &container_of,
                &blocks,
                &display_by_id,
                &tags_by,
                &a,
                &mut seen,
                None,
            );
        }
        // Belt and braces. The roots filter above only counts containers that
        // are themselves in the rendered set, so an orphaned slice is already a
        // root and already printed — this catches the one case that filter
        // cannot: a containment cycle, where every node has an in-set container
        // and nothing qualifies as a root. Unreachable while the write path's
        // cycle check holds, which is exactly why it is cheap to keep.
        for row in &tasks {
            if !seen.contains(&row.id) {
                print_node(
                    row,
                    0,
                    &by_id,
                    &contains,
                    &container_of,
                    &blocks,
                    &display_by_id,
                    &tags_by,
                    &a,
                    &mut seen,
                    None,
                );
            }
        }
    }
    Ok(())
}

/// Display-ids for a list of task ids, in id order.
///
/// The `T<id>` fallback is unreachable in practice — `display_by_id` is built
/// from every task in the store, not just the rendered ones — and exists so a
/// referent that somehow escapes the lookup still prints as an edge rather than
/// vanishing.
fn dids(ids: Option<&Vec<i64>>, display_by_id: &HashMap<i64, String>) -> Vec<String> {
    ids.map(|v| {
        v.iter()
            .map(|d| {
                display_by_id
                    .get(d)
                    .cloned()
                    .unwrap_or_else(|| format!("T{d}"))
            })
            .collect()
    })
    .unwrap_or_default()
}

/// One row plus, indented beneath it, whatever it contains.
///
/// `seen` guards against printing a task twice when it sits in two containers,
/// and bounds the walk even if a containment cycle ever slipped past the write
/// path's check — a renderer that hangs is worse than one that prints a task
/// under only its first container.
///
/// Printing once is not the same as hiding the rest: a multi-contained task
/// carries an `(also in …)` note, because otherwise its second container renders
/// as empty and the global tree silently disagrees with `qp tree <that
/// container>`. Nothing forbids multiple containment, so the renderer has to
/// say so rather than pick a winner in silence.
#[allow(clippy::too_many_arguments)]
fn print_node(
    row: &store::TaskRow,
    depth: usize,
    by_id: &HashMap<i64, &store::TaskRow>,
    contains: &HashMap<i64, Vec<i64>>,
    container_of: &HashMap<i64, Vec<i64>>,
    blocks: &HashMap<i64, Vec<i64>>,
    display_by_id: &HashMap<i64, String>,
    tags_by: &HashMap<i64, Vec<String>>,
    a: &TreeArgs,
    seen: &mut HashSet<i64>,
    parent: Option<i64>,
) {
    if !seen.insert(row.id) {
        return;
    }
    let dep_s = dids(blocks.get(&row.id), display_by_id).join(",");
    let dep_part = if dep_s.is_empty() {
        String::new()
    } else {
        format!(" <- [{dep_s}]")
    };
    let tag_part = if a.show_tags {
        let ts = tags_by
            .get(&row.id)
            .map(|v| v.join(","))
            .unwrap_or_default();
        if ts.is_empty() {
            String::new()
        } else {
            format!(" #{ts}")
        }
    } else {
        String::new()
    };
    // Every container except the one we are printing under. At depth 0 the task
    // is not under any of them, so they all count.
    let others: Vec<String> = container_of
        .get(&row.id)
        .into_iter()
        .flatten()
        .filter(|p| by_id.contains_key(p) && (depth == 0 || Some(**p) != parent))
        .filter_map(|p| display_by_id.get(p).cloned())
        .collect();
    let also_part = if others.is_empty() {
        String::new()
    } else {
        format!(" (also in {})", others.join(","))
    };
    let tier_s = row.tier.as_deref().unwrap_or("-");
    let indent = "  ".repeat(depth);
    let (did, state, title) = (&row.display_id, &row.state, &row.title);
    println!("{did:>5}  {state:<9}  {tier_s:<8}  {indent}{title}{dep_part}{also_part}{tag_part}");
    if a.with_description {
        if let Some(d) = row.description.as_deref().filter(|s| !s.is_empty()) {
            for line in crate::cmd::show::wrap_text(d, 80).iter().take(3) {
                println!("       {indent}{line}");
            }
        }
    }
    for child in contains.get(&row.id).into_iter().flatten() {
        if let Some(c) = by_id.get(child) {
            print_node(
                c,
                depth + 1,
                by_id,
                contains,
                container_of,
                blocks,
                display_by_id,
                tags_by,
                a,
                seen,
                Some(row.id),
            );
        }
    }
}
