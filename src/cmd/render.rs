//! Shared human-readable rendering of event payloads.
//!
#![doc = include_str!("../../docs/modules/render.md")]

use serde_json::Value;

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
