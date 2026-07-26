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
    let mut s = conn
        .prepare("SELECT task_id, depends_on_task_id, mode FROM dep ORDER BY depends_on_task_id")?;
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
                &blocks,
                &display_by_id,
                &tags_by,
                &a,
                &mut seen,
            );
        }
        // A task contained by something outside the rendered set still has a
        // container, so it is not a root, and nothing above printed it. Without
        // this it would be silently dropped.
        for row in &tasks {
            if !seen.contains(&row.id) {
                print_node(
                    row,
                    0,
                    &by_id,
                    &contains,
                    &blocks,
                    &display_by_id,
                    &tags_by,
                    &a,
                    &mut seen,
                );
            }
        }
    }
    Ok(())
}

/// Display-ids for a list of task ids, in id order.
///
/// Falls back to `T<id>` for a referent outside the rendered set, matching what
/// the human rows do rather than dropping the edge.
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
#[allow(clippy::too_many_arguments)]
fn print_node(
    row: &store::TaskRow,
    depth: usize,
    by_id: &HashMap<i64, &store::TaskRow>,
    contains: &HashMap<i64, Vec<i64>>,
    blocks: &HashMap<i64, Vec<i64>>,
    display_by_id: &HashMap<i64, String>,
    tags_by: &HashMap<i64, Vec<String>>,
    a: &TreeArgs,
    seen: &mut HashSet<i64>,
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
    let tier_s = row.tier.as_deref().unwrap_or("-");
    let indent = "  ".repeat(depth);
    let (did, state, title) = (&row.display_id, &row.state, &row.title);
    println!("{did:>5}  {state:<9}  {tier_s:<8}  {indent}{title}{dep_part}{tag_part}");
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
                blocks,
                display_by_id,
                tags_by,
                a,
                seen,
            );
        }
    }
}
