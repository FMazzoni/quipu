use anyhow::Result;
use clap::Args;
use serde::Serialize;
use crate::{db, id};

#[derive(Args, Debug)]
pub struct AddArgs {
    pub title: String,
    #[arg(long)] pub tier: Option<String>,
    #[arg(long)] pub description: Option<String>,
    #[arg(long = "depends-on", value_name = "TASK_ID")]
    pub depends_on: Vec<String>,
    #[arg(long, value_name = "NAME")]
    pub tag: Vec<String>,
    #[arg(long)] pub json: bool,
}

#[derive(Serialize)]
struct Created {
    display_id: String,
    title: String,
    state: String,
    tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    tags: Vec<String>,
}
impl std::fmt::Display for Created {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\t{}\t{}", self.display_id, self.state, self.title)
    }
}

pub fn run(db_path: &std::path::Path, a: AddArgs) -> Result<()> {
    let mut conn = db::open(db_path)?;
    // Pre-resolve deps outside tx; errors early.
    let mut dep_ids = Vec::with_capacity(a.depends_on.len());
    for d in &a.depends_on { dep_ids.push(id::resolve(&conn, d)?); }

    let created = db::with_tx(&mut conn, |tx| {
        let state = if dep_ids.is_empty() { db::STATE_READY } else { db::STATE_PENDING };
        tx.execute(
            "INSERT INTO task(display_id, title, tier, description, state) VALUES ('', ?, ?, ?, ?)",
            rusqlite::params![a.title, a.tier, a.description, state])?;
        let row = tx.last_insert_rowid();
        let prefix = db::display_prefix(tx)?;
        let display = id::encode(row, &prefix);
        tx.execute("UPDATE task SET display_id = ? WHERE id = ?",
            rusqlite::params![display, row])?;
        for did in &dep_ids {
            // Cycle check: dep edges only go from `row` outward, so cycle only possible if
            // some existing edge from *did to row exists. Use would_cycle for safety.
            if db::would_cycle(tx, row, *did)? {
                return Err(db::constraint(format!(
                    "cycle: {} depends on dep#{} which (transitively) depends on {}",
                    display, did, display)));
            }
            tx.execute("INSERT INTO dep(task_id, depends_on_task_id) VALUES (?,?)",
                rusqlite::params![row, did])?;
        }
        for tag in &a.tag {
            tx.execute("INSERT OR IGNORE INTO tag(task_id, name) VALUES (?,?)",
                rusqlite::params![row, tag])?;
        }
        // If deps were added, run refresh_ready now: if all deps are already done/cancelled
        // this task may immediately transition to ready.
        if !dep_ids.is_empty() { db::refresh_ready(tx)?; }
        let actual_state: String = tx.query_row(
            "SELECT state FROM task WHERE id = ?", [row], |r| r.get(0))?;
        db::insert_event(tx, Some(row), "state_change", None,
            Some(&serde_json::json!({"to": actual_state, "title": a.title})))?;
        Ok(Created {
            display_id: display, title: a.title.clone(),
            state: actual_state, tier: a.tier.clone(),
            description: a.description.clone(),
            tags: a.tag.clone(),
        })
    })?;

    if a.json { println!("{}", serde_json::to_string(&created)?); }
    else      { println!("{created}"); }
    Ok(())
}
