//! Remote attachment: the ledger url in `meta`, and the shape a url must have to be one.
//!
#![doc = include_str!("../docs/modules/remote_config.md")]

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

/// `meta` key holding the ledger sync url.
pub const KEY_LEDGER: &str = "remote.ledger";

/// What a store may be attached to. `None` is the local-only state.
///
/// A struct rather than a bare `Option<String>` because attachment is a set of
/// urls that has been two before and will be again — the git claim authority
/// was the second one until claims moved back to a single writer, and the
/// server url is the next. Collapsing it the moment it holds one field would
/// have to be undone, and every caller re-taught, the day it holds two.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Remotes {
    pub ledger: Option<String>,
}

impl Remotes {
    /// True when nothing is configured — the default, and a first-class state.
    pub fn local_only(&self) -> bool {
        self.ledger.is_none()
    }
}

/// Reads the attachment out of `meta`.
///
/// A missing row is `None`, which is what makes an unconfigured store cost one
/// local `SELECT` and nothing else.
pub fn load(conn: &Connection) -> Result<Remotes> {
    Ok(Remotes {
        ledger: read_key(conn, KEY_LEDGER)?,
    })
}

fn read_key(conn: &Connection, key: &str) -> Result<Option<String>> {
    let v: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?;
    Ok(v.filter(|s| !s.is_empty()))
}

/// Upserts one url. Config only — no network, no validation of the far end.
pub fn set(conn: &Connection, key: &str, url: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, url],
    )?;
    Ok(())
}

/// Deletes one url. Returns whether a row was actually removed.
pub fn clear(conn: &Connection, key: &str) -> Result<bool> {
    let n = conn.execute("DELETE FROM meta WHERE key = ?1", [key])?;
    Ok(n == 1)
}

/// Rejects a url that cannot survive being passed on as an argument.
///
/// Deliberately shallow, and deliberately scheme-blind: this is the half of
/// validation that is true of every url quipu will ever store, so a new kind of
/// remote adds its own shape check on top rather than editing this one. What it
/// catches is the input that would silently misbehave — empty, padded with
/// whitespace, carrying a control character, or starting with `-` and so read
/// as a flag by whatever the url is eventually handed to.
pub fn validate_url(url: &str) -> Result<()> {
    if url.is_empty() {
        return Err(crate::db::invalid_input("remote url must not be empty"));
    }
    if url.trim() != url {
        return Err(crate::db::invalid_input(
            "remote url has leading or trailing whitespace",
        ));
    }
    if url.chars().any(|c| c.is_control()) {
        return Err(crate::db::invalid_input(
            "remote url contains a control character",
        ));
    }
    if url.starts_with('-') {
        return Err(crate::db::invalid_input(
            "remote url must not start with '-' (it would be read as a flag)",
        ));
    }
    Ok(())
}

/// Outcome of a reachability probe. Opt-in everywhere; nothing runs it implicitly.
///
/// `Fault` carries the object store's own diagnosis rather than collapsing to
/// `Unreachable`. That distinction is the whole value of the probe on a ledger:
/// missing credentials, rejected credentials, a missing bucket, a prefix outside
/// your policy, a TLS trust failure and a skewed clock all want different fixes,
/// and they are indistinguishable once flattened into "could not reach it".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    Ok,
    /// The far end answered, and said why it would not serve the request.
    Fault(String),
}

/// Asks the far end whether it is there. The only function in this module that touches the network.
///
/// A LIST of the ledger's own prefix is the cheapest question that exercises
/// credentials, trust, the bucket and the policy in one round trip.
pub fn probe(url: &str) -> Reach {
    let client = match crate::s3::Client::open(url) {
        Ok(c) => c,
        Err(e) => return Reach::Fault(e.to_string()),
    };
    match client.list("") {
        Ok(_) => Reach::Ok,
        Err(crate::s3::Fault::Unreachable { detail, .. }) => {
            Reach::Fault(format!("unreachable: {detail}"))
        }
        Err(e) => Reach::Fault(e.to_string()),
    }
}

/// Rejects a ledger url that is not an object-store url.
///
/// The ledger is object storage and nothing else. A git url here used to work
/// and no longer does: a fetch takes a whole branch, so every store carried
/// every other project's history, and no transport option turned that off. The
/// refusal names the form rather than saying "invalid", because the person
/// typing it has a working git url in their hand.
pub fn validate_ledger_url(url: &str) -> Result<()> {
    validate_url(url)?;
    crate::s3::parse(url)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_only_is_the_default() {
        assert!(Remotes::default().local_only());
    }

    #[test]
    fn validate_url_rejects_the_dangerous_shapes() {
        assert!(validate_url("").is_err());
        assert!(validate_url(" host.invalid/x").is_err());
        assert!(validate_url("--upload-pack=evil").is_err());
        assert!(validate_url("host.invalid:x\ny").is_err());
        assert!(validate_url("s3://host.invalid/bucket").is_ok());
        assert!(validate_url("/srv/ledger").is_ok());
    }

    /// The shallow check runs before the object-store shape check, not after.
    ///
    /// The order is what makes the two composable: a scheme this build has
    /// never heard of must be rejected as the wrong *kind* of url, and a url
    /// that would be read as a flag must be rejected before anything tries to
    /// parse a host out of it.
    #[test]
    fn a_ledger_url_must_pass_the_shallow_check_and_then_be_an_object_url() {
        assert!(validate_ledger_url("--upload-pack=evil").is_err());
        assert!(validate_ledger_url("ssh://host.invalid/srv/x.git").is_err());
        assert!(validate_ledger_url("s3://host.invalid/bucket/prefix").is_ok());
    }
}
