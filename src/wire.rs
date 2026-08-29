//! The HTTP projection of the error taxonomy, in both directions.
//!
#![doc = include_str!("../docs/modules/wire.md")]

use crate::db::QuipuError;
use serde_json::Value;

/// Path prefix every served verb sits under.
///
/// Versioned so a second grammar can coexist rather than replace this one.
pub const API_PREFIX: &str = "/v1/";

/// The liveness probe.
///
/// Deliberately outside the verb namespace, so a project named `healthz` can
/// never collide with it.
pub const HEALTH_PATH: &str = "/healthz";

/// Status and `{"error": ...}` body for a failure the server is about to write.
///
/// The body is byte-for-byte what `main.rs` already writes to stderr under
/// `--json`; only the status is new. Anything that does not downcast to
/// [`QuipuError`] is `internal`/500, matching the CLI's own fallback.
pub fn failure(e: &anyhow::Error) -> (u16, Value) {
    match e.downcast_ref::<QuipuError>() {
        Some(q) => (
            status_for(q.kind()),
            serde_json::json!({ "error": q.to_json() }),
        ),
        None => (
            500,
            serde_json::json!({"error": {"kind": "internal", "message": format!("{e:#}")}}),
        ),
    }
}

/// The lossy projection of a `kind` onto an HTTP status.
///
/// Lossy on purpose: `not_found` and `no_store` share 404, `conflict` and
/// `not_owner` share 409. `kind` is the contract and a client must branch on
/// it; the status exists for proxies and `curl`. The mapping is the table in
/// `docs/wire-contract.md` and changing a row is a published-contract change.
///
/// `unavailable` has no row because the server never produces it — a process
/// that answered is proof the front door was reached. It lands in the `_` arm
/// as 500 only so this function is total.
pub fn status_for(kind: &str) -> u16 {
    match kind {
        "invalid_input" => 400,
        "not_found" | "no_store" => 404,
        "conflict" | "not_owner" => 409,
        "invariant" => 422,
        _ => 500,
    }
}

/// Rebuilds the error a server reported, so the client can render and exit as
/// if the store were local.
///
/// This is the inverse of [`failure`] and the property the whole contract rests
/// on: a conflict is exit 2 whether the store was on this disk or behind a
/// socket, so an orchestrator that retries on 2 needs no change at all. The
/// reconstructed value goes back through `main.rs`'s existing rendering path.
///
/// `kind` is read from the body, never inferred from the status — the status
/// map is not injective and inverting it would collapse `no_store` into
/// `not_found` and silently turn exit 6 into exit 2.
pub fn error_from(status: u16, body: &str, endpoint: &str) -> anyhow::Error {
    // A gateway status can only have been written by something between the
    // caller and quipu: the process itself is forbidden from sending 5xx of
    // this shape, so an answering front door is ruled out and `unavailable` is
    // exactly right. See the wire contract's "503 is reserved" section.
    if matches!(status, 502..=504) {
        return unavailable(
            format!("{endpoint} answered {status}; no store saw the request"),
            endpoint,
        );
    }
    let Some(err) = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").cloned())
    else {
        return anyhow::anyhow!(
            "{endpoint} answered {status} with a body that is not the error envelope: \
             {}",
            truncate(body)
        );
    };
    let s = |k: &str| err.get(k).and_then(Value::as_str).map(str::to_string);
    let message = s("message").unwrap_or_else(|| format!("{endpoint} answered {status}"));
    let task = s("task");
    match err.get("kind").and_then(Value::as_str).unwrap_or("") {
        "conflict" => anyhow::Error::from(QuipuError::Conflict {
            code: intern(s("code").unwrap_or_default()),
            message,
            task,
        }),
        "not_owner" => anyhow::Error::from(QuipuError::NotOwner {
            message,
            task,
            owner: s("owner"),
        }),
        "not_found" => anyhow::Error::from(QuipuError::NotFound { message, task }),
        "invariant" => anyhow::Error::from(QuipuError::Invariant {
            code: intern(s("code").unwrap_or_default()),
            message,
        }),
        "invalid_input" => anyhow::Error::from(QuipuError::InvalidInput(message)),
        "no_store" => anyhow::Error::from(QuipuError::NoStore {
            message,
            path: s("path"),
        }),
        "unavailable" => unavailable(message, endpoint),
        // An unrecognised kind is `internal` (exit 1), not a guess. Exit 1 is
        // documented as "a bug", and a client that cannot name the failure it
        // was handed is exactly that.
        _ => anyhow::anyhow!("{message}"),
    }
}

/// The transport failure: the front door could not be reached. Exit 7.
pub fn unavailable(message: impl Into<String>, endpoint: &str) -> anyhow::Error {
    anyhow::Error::from(QuipuError::Unavailable {
        message: message.into(),
        endpoint: Some(endpoint.to_string()),
    })
}

/// Gives a wire-supplied `code` the `&'static str` the taxonomy stores.
///
/// The codes are an open, additively-growing set — the wire contract says so —
/// so there is no closed table to match against and no honest way to avoid a
/// leak. It is bounded by construction: a client process issues one request and
/// then exits, so this runs at most once per invocation, and `code` is a short
/// identifier the server chose from its own source. Widening `QuipuError`'s
/// `code` to `Cow<'static, str>` was the alternative and it rewrites every
/// construction site in the tree to pay for a byte that is freed by `exit`.
fn intern(code: String) -> &'static str {
    Box::leak(code.into_boxed_str())
}

fn truncate(s: &str) -> String {
    let s = s.trim();
    if s.len() <= 200 {
        return s.to_string();
    }
    format!("{}…", &s[..200])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every documented row of the status table, in both directions.
    ///
    /// The reverse direction is what an agent's exit-code branch depends on:
    /// the kind carried in the body must survive the round trip, because the
    /// exit code is derived from it and not from the status.
    #[test]
    fn the_status_map_is_the_documented_table() {
        let rows: &[(&str, u16, i32)] = &[
            ("invalid_input", 400, 1),
            ("not_found", 404, 2),
            ("no_store", 404, 6),
            ("conflict", 409, 2),
            ("not_owner", 409, 2),
            ("invariant", 422, 2),
            ("internal", 500, 1),
        ];
        for (kind, status, exit) in rows {
            assert_eq!(status_for(kind), *status, "status for {kind}");
            let body = serde_json::json!({"error": {"kind": kind, "message": "m", "code": "c"}})
                .to_string();
            let e = error_from(*status, &body, "http://127.0.0.1:1/v1/x");
            let got = e
                .downcast_ref::<QuipuError>()
                .map_or(1, QuipuError::exit_code);
            assert_eq!(got, *exit, "exit code for {kind} after a round trip");
            if let Some(q) = e.downcast_ref::<QuipuError>() {
                assert_eq!(q.kind(), *kind, "kind survives the round trip");
            }
        }
    }

    /// A gateway status is a transport failure, whatever body came with it.
    #[test]
    fn gateway_statuses_become_unavailable() {
        for status in [502u16, 503, 504] {
            let e = error_from(status, "<html>oops</html>", "http://127.0.0.1:1/v1/add");
            let q = e.downcast_ref::<QuipuError>().expect("typed");
            assert_eq!(q.kind(), "unavailable");
            assert_eq!(q.exit_code(), 7);
        }
    }

    /// A body that is not the envelope is `internal`, never a guessed kind.
    #[test]
    fn an_unparseable_body_is_internal() {
        let e = error_from(409, "not json at all", "http://127.0.0.1:1/v1/claim");
        assert!(e.downcast_ref::<QuipuError>().is_none());
    }
}
