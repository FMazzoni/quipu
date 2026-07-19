//! Shared human-readable rendering of event payloads.
//!
//! Used by `timeline`, `show` and `report` so that one event kind reads the
//! same wherever it appears.

use serde_json::Value;

/// Render a short, human-readable summary of an event's payload for a given
/// event `kind`. Shared by `timeline`, `show`, and `report`.
pub fn summarize_payload(kind: &str, p: &Value) -> String {
    match kind {
        "state_change" => format!("→ {}", p.get("to").and_then(|v| v.as_str()).unwrap_or("")),
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
