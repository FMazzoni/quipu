//! The client half of the front door: one blocking request, one payload back.
//!
#![doc = include_str!("../docs/modules/client.md")]

use crate::wire;
use anyhow::Result;
use serde_json::Value;

/// Ceiling on a response body, so a wrong endpoint cannot exhaust memory.
const MAX_BODY: u64 = 8 * 1024 * 1024;

/// Ceiling on a health body, which is four short fields and a list of names.
///
/// Much smaller than [`MAX_BODY`] on purpose: the probe is aimed at a url that
/// may not be a quipu front door at all, so the first thing it can meet is a
/// large page from some other service.
const MAX_HEALTH_BODY: u64 = 64 * 1024;

/// Issues one served verb against `base` and returns the success payload.
///
/// Every non-2xx answer and every transport failure comes back as the typed
/// error the CLI would have produced locally, so the caller's exit code is
/// unchanged by there being a socket in the path.
pub fn call(base: &str, verb: &str, project: Option<&str>, body: Value) -> Result<Value> {
    let base = check_url(base)?;
    let mut url = format!("{base}{}{verb}", wire::API_PREFIX);
    if let Some(p) = project {
        url.push_str("?project=");
        url.push_str(&encode(p));
    }
    // `http_status_as_error(false)` for the same reason `s3.rs` sets it: the
    // response body is the only place the cause lives, and turning a 409 into
    // an opaque status error throws away the `kind` the whole taxonomy rests on.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .post(&url)
        .content_type("application/json")
        .send(serde_json::to_vec(&body)?)
        .map_err(|e| wire::unavailable(format!("{url}: {e}"), &url))?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_string()
        // A short read after the request was sent is the middle restart window
        // in the process contract: the write may or may not have landed, so it
        // is `unavailable` (exit 7) and never a conflict.
        .map_err(|e| wire::unavailable(format!("{url}: truncated response: {e}"), &url))?;
    if (200..300).contains(&status) {
        return Ok(serde_json::from_str(&text)?);
    }
    Err(wire::error_from(status, &text, &url))
}

/// Asks the front door what it is and what it serves. `GET /healthz`, no body.
///
/// The one request in this module that is not a verb. It is a GET because the
/// endpoint is a liveness probe rather than a mutation, and it takes no project
/// parameter because the process contract forbids one — the caller compares the
/// returned list locally instead. A non-2xx or an unreadable body comes back as
/// the same typed error a verb would produce, so the caller can tell a
/// transport failure from an answer it did not understand.
pub fn health(base: &str) -> Result<Value> {
    let base = check_url(base)?;
    let url = format!("{base}{}", wire::HEALTH_PATH);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .get(&url)
        .call()
        .map_err(|e| wire::unavailable(format!("{url}: {e}"), &url))?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .with_config()
        .limit(MAX_HEALTH_BODY)
        .read_to_string()
        .map_err(|e| wire::unavailable(format!("{url}: truncated response: {e}"), &url))?;
    if !(200..300).contains(&status) {
        return Err(wire::error_from(status, &text, &url));
    }
    // Deliberately NOT `unavailable`: something answered, so the front door was
    // reached and the caller must not read this as "nothing is listening".
    serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("{url} answered {status} with a body that is not JSON: {e}"))
}

/// Validates the endpoint and strips its trailing slash.
///
/// The plaintext rule is the wire contract's, and it reuses
/// `s3::is_loopback_literal` rather than growing a second opinion about when
/// plaintext is acceptable. Writing `http://` in `--server` *is* the explicit
/// request for plaintext; what it cannot do is make a non-loopback host
/// acceptable, and a name that merely resolves to loopback does not count —
/// a DNS answer is attacker-influenceable and is not a security property.
pub fn check_url(base: &str) -> Result<String> {
    let rest = match base.strip_prefix("https://") {
        Some(r) => r,
        None => {
            let Some(r) = base.strip_prefix("http://") else {
                return Err(crate::db::invalid_input(format!(
                    "`{base}` is not a server url; expected http:// or https://"
                )));
            };
            let authority = r.split(['/', '?']).next().unwrap_or(r);
            if !crate::s3::is_loopback_literal(authority) {
                return Err(crate::db::invalid_input(format!(
                    "refusing plaintext http to `{authority}`; use https, or terminate \
                     TLS in front of the server and point at loopback"
                )));
            }
            r
        }
    };
    if rest.is_empty() {
        return Err(crate::db::invalid_input(format!("`{base}` names no host")));
    }
    Ok(base.trim_end_matches('/').to_string())
}

/// Percent-encodes a project name for the query string.
///
/// Names are already constrained by `project::validate_name`, so this covers
/// the operator who typed one that is not; it is a correctness guard, not a
/// general encoder.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plaintext is loopback-only, and a resolving name is not loopback.
    #[test]
    fn plaintext_is_refused_off_loopback() {
        assert!(check_url("http://127.0.0.1:8787").is_ok());
        assert!(check_url("http://[::1]:8787").is_ok());
        assert!(check_url("http://localhost:8787").is_ok());
        assert!(check_url("https://qp.example.com").is_ok());
        for bad in [
            "http://qp.example.com",
            "http://host.invalid:8787",
            "ftp://127.0.0.1",
            "127.0.0.1:8787",
        ] {
            assert!(check_url(bad).is_err(), "{bad} must be refused");
        }
    }

    /// A trailing slash must not produce a double slash in the path.
    #[test]
    fn a_trailing_slash_is_stripped() {
        assert_eq!(
            check_url("http://127.0.0.1:1/").unwrap(),
            "http://127.0.0.1:1"
        );
    }

    #[test]
    fn project_names_are_encoded() {
        assert_eq!(encode("quipu-main"), "quipu-main");
        assert_eq!(encode("a b&c"), "a%20b%26c");
    }
}
