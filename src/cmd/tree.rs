//! Render the dependency DAG.
//!
#![doc = include_str!("../../docs/modules/tree.md")]

use crate::outcome::Outcome;
use crate::{db, id, store};
use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Args, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeArgs {
    /// Optional task id — when present, restrict output to this task + its transitive deps.
    #[serde(default)]
    pub task: Option<String>,
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
    #[arg(long)]
    #[serde(default)]
    pub tier: Option<String>,
    #[arg(long)]
    #[serde(skip)]
    pub show_tags: bool,
    /// Print each task's description on indented continuation lines.
    #[arg(long = "with-description")]
    #[serde(skip)]
    pub with_description: bool,
}

impl crate::route::Served for TreeArgs {
    const VERB: &'static str = "tree";
    type Out = TreeView;
    fn wants_json(&self) -> bool {
        self.json
    }
    fn exec(db_path: &std::path::Path, a: Self) -> Result<Self::Out> {
        exec(db_path, a)
    }
    fn restore_view(&self, out: &mut Self::Out) {
        out.show_tags = self.show_tags;
        out.with_description = self.with_description;
    }
}

/// Every task in scope, each carrying its own edges as display ids.
///
/// One endpoint, not one per query. Building this view locally is eight reads —
/// the task query, the dep table, the display-id map, the tag table, and the
/// bulk helpers behind them — and a wire protocol that exposed those separately
/// would turn one `qp tree` into eight round trips over a link whose latency
/// dwarfs every one of the queries. The nesting is reconstructed in `human`
/// from `contains` and `part_of`, which is why the client needs no second call
/// to draw the hierarchy.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct TreeView {
    #[serde(deserialize_with = "rows_are_an_array")]
    rows: Value,
    #[serde(skip)]
    show_tags: bool,
    #[serde(skip)]
    with_description: bool,
}

/// Refuses a response body that is not the array this view renders.
///
/// The whole argument is on `wire::expect_array`; the short version is that a
/// `#[serde(transparent)]` newtype over `Value` accepts any JSON at all, and
/// every one of these views renders a mismatch as *empty* rather than as a
/// failure. `deserialize_with` keeps the payload a `Value` — so the bytes and
/// their key order are untouched — and adds the one refusal.
fn rows_are_an_array<'de, D>(d: D) -> std::result::Result<Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    crate::wire::expect_array(Value::deserialize(d)?).map_err(D::Error::custom)
}

/// A minimal valid sample; see `list.rs` for why this is not derived.
#[cfg(test)]
impl Default for TreeView {
    fn default() -> Self {
        TreeView {
            rows: Value::Array(vec![]),
            show_tags: false,
            with_description: false,
        }
    }
}

impl Outcome for TreeView {
    fn human(&self) -> String {
        let rows: Vec<&Value> = self.rows.as_array().into_iter().flatten().collect();
        let by_did: HashMap<&str, &Value> = rows
            .iter()
            .filter_map(|r| r["display_id"].as_str().map(|d| (d, *r)))
            .collect();
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        // Roots are everything with no container *inside the rendered set*: a
        // slice whose wave was filtered out by `--tier` must still print, at
        // the top level, rather than vanish with its parent.
        for r in &rows {
            let contained = strs(r, "part_of")
                .into_iter()
                .any(|p| by_did.contains_key(p));
            if !contained {
                self.node(r, 0, &by_did, &mut seen, None, &mut out);
            }
        }
        // Belt and braces. The roots filter above only counts containers that
        // are themselves in the rendered set, so an orphaned slice is already a
        // root and already printed — this catches the one case that filter
        // cannot: a containment cycle, where every node has an in-set container
        // and nothing qualifies as a root. Unreachable while the write path's
        // cycle check holds, which is exactly why it is cheap to keep.
        for r in &rows {
            self.node(r, 0, &by_did, &mut seen, None, &mut out);
        }
        out.join("\n")
    }
}

/// The display ids under `key`, as borrowed strs in their stored order.
fn strs<'a>(v: &'a Value, key: &str) -> Vec<&'a str> {
    v[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

impl TreeView {
    /// One row plus, indented beneath it, whatever it contains.
    ///
    /// `seen` guards against printing a task twice when it sits in two
    /// containers, and bounds the walk even if a containment cycle ever slipped
    /// past the write path's check — a renderer that hangs is worse than one
    /// that prints a task under only its first container.
    ///
    /// Printing once is not the same as hiding the rest: a multi-contained task
    /// carries an `(also in …)` note, because otherwise its second container
    /// renders as empty and the global tree silently disagrees with
    /// `qp tree <that container>`. Nothing forbids multiple containment, so the
    /// renderer has to say so rather than pick a winner in silence.
    fn node(
        &self,
        row: &Value,
        depth: usize,
        by_did: &HashMap<&str, &Value>,
        seen: &mut HashSet<String>,
        parent: Option<&str>,
        out: &mut Vec<String>,
    ) {
        let did = row["display_id"].as_str().unwrap_or("");
        if !seen.insert(did.to_string()) {
            return;
        }
        let dep_s = strs(row, "blocked_by").join(",");
        let dep_part = if dep_s.is_empty() {
            String::new()
        } else {
            format!(" <- [{dep_s}]")
        };
        let tag_part = if self.show_tags {
            let ts = strs(row, "tags").join(",");
            if ts.is_empty() {
                String::new()
            } else {
                format!(" #{ts}")
            }
        } else {
            String::new()
        };
        // Every container except the one we are printing under. At depth 0 the
        // task is not under any of them, so they all count.
        let others: Vec<&str> = strs(row, "part_of")
            .into_iter()
            .filter(|p| by_did.contains_key(p) && (depth == 0 || Some(*p) != parent))
            .collect();
        let also_part = if others.is_empty() {
            String::new()
        } else {
            format!(" (also in {})", others.join(","))
        };
        let tier_s = row["tier"].as_str().unwrap_or("-");
        let indent = "  ".repeat(depth);
        let state = row["state"].as_str().unwrap_or("");
        let title = row["title"].as_str().unwrap_or("");
        out.push(format!(
            "{did:>5}  {state:<9}  {tier_s:<8}  {indent}{title}{dep_part}{also_part}{tag_part}"
        ));
        if self.with_description {
            if let Some(d) = row["description"].as_str().filter(|s| !s.is_empty()) {
                for line in crate::cmd::show::wrap_text(d, 80).iter().take(3) {
                    out.push(format!("       {indent}{line}"));
                }
            }
        }
        for child in strs(row, "contains") {
            if let Some(c) = by_did.get(child) {
                self.node(c, depth + 1, by_did, seen, Some(did), out);
            }
        }
    }
}

pub fn exec(db_path: &std::path::Path, a: TreeArgs) -> Result<TreeView> {
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

    // Unconditional, where the old human path skipped it unless `--show-tags`.
    // The payload is the same bytes whether the caller wanted prose or JSON —
    // that is the wire contract's rule, and it costs one indexed table scan the
    // `--json` path already paid on every run.
    let mut tags_by: HashMap<i64, Vec<String>> = HashMap::new();
    let mut s = conn.prepare("SELECT task_id, name FROM tag")?;
    for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (t, n) = r?;
        tags_by.entry(t).or_default().push(n);
    }

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
                .insert("description".into(), Value::String(d.clone()));
        }
        out.push(obj);
    }
    Ok(TreeView {
        rows: Value::Array(out),
        show_tags: a.show_tags,
        with_description: a.with_description,
    })
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
