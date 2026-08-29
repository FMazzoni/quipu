//! Shared human-readable rendering of event payloads.
//!
#![doc = include_str!("../../docs/modules/render.md")]

use serde_json::Value;

/// The wire and `--json` shape of one event-log row.
///
/// Three views emit it byte-identically — `timeline`, `decisions`, and
/// `report`'s board payload — and they did so from three hand-written copies of
/// this `json!`. That is the divergence risk `store.rs` names for its queries,
/// one layer up: an added field that reaches two of the three is a shape a
/// consumer cannot rely on.
///
/// Deriving `Serialize` on `store::EventRow` was the alternative and it is
/// rejected here, not merely unimplemented: a derive emits fields in
/// declaration order, `json!` builds a `serde_json::Map` that orders them
/// alphabetically, and switching would rewrite the bytes of every existing
/// `--json` consumer's input for no gain.
pub fn event_json(e: &crate::store::EventRow) -> Value {
    serde_json::json!({
        "id": e.id,
        "task": e.task,
        "ts": e.ts,
        "kind": e.kind,
        "agent_id": e.agent,
        "payload": e.payload,
    })
}

/// Render an event payload as a short human-readable line, keyed on `kind`.
///
/// Shared by `timeline` and `show`.
pub fn summarize_payload(kind: &str, p: &Value) -> String {
    match kind {
        "state_change" => {
            let to = p.get("to").and_then(|v| v.as_str()).unwrap_or("");
            // `from` reads as an origin only when it is a string. A `null` is a
            // task's creation — no prior state — and an absent key is an event
            // written before origins were recorded at all. Both render as the
            // bare arrow, because neither has an origin to name and inventing a
            // placeholder for the second would be the renderer asserting
            // something the log does not say.
            match p.get("from").and_then(|v| v.as_str()) {
                Some(from) => format!("{from} → {to}"),
                None => format!("→ {to}"),
            }
        }
        "decision" => {
            let text = p.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if p.get("auto").and_then(|v| v.as_bool()).unwrap_or(false) {
                format!("[auto] {text}")
            } else {
                text.to_string()
            }
        }
        "dep_added" | "dep_removed" => p
            .get("on")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "tag_added" | "tag_removed" => p
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "blocker" => p
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "edit" => {
            if let Some(obj) = p.get("changes").and_then(|v| v.as_object()) {
                obj.keys().cloned().collect::<Vec<_>>().join(",")
            } else {
                String::new()
            }
        }
        _ => {
            let s = serde_json::to_string(p).unwrap_or_default();
            if s.chars().count() > 80 {
                let truncated: String = s.chars().take(80).collect();
                format!("{truncated}...")
            } else {
                s
            }
        }
    }
}
