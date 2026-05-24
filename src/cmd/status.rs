use anyhow::Result;
use clap::Args;
use crate::db;

#[derive(Args, Debug)]
pub struct StatusArgs {
    #[arg(long)] pub json: bool,
}

pub fn run(db_path: &std::path::Path, a: StatusArgs) -> Result<()> {
    let conn = db::open(db_path)?;
    let mut stmt = conn.prepare("SELECT state, COUNT(*) FROM task GROUP BY state ORDER BY state")?;
    let rows: Vec<(String, i64)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut map = serde_json::Map::new();
    for (state, count) in &rows {
        map.insert(state.clone(), serde_json::Value::from(*count));
    }
    // Always include all known states (zero if absent) so consumers can rely on the keys.
    for s in ["pending","ready","assigned","running","done","blocked","cancelled"] {
        map.entry(s.to_string()).or_insert(serde_json::Value::from(0));
    }
    if a.json {
        println!("{}", serde_json::to_string(&serde_json::Value::Object(map))?);
    } else {
        for (state, count) in &rows {
            println!("{state:<10} {count}");
        }
    }
    Ok(())
}
