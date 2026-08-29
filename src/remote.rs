//! Remote attachment: the ledger and front-door urls in `meta`, and the shape each must have.
//!
#![doc = include_str!("../docs/modules/remote_config.md")]

use anyhow::Result;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::Path;

/// `meta` key holding the ledger sync url.
pub const KEY_LEDGER: &str = "remote.ledger";

/// `meta` key holding the front-door url this store is reached through.
pub const KEY_SERVER: &str = "remote.server";

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
    pub server: Option<String>,
}

impl Remotes {
    /// True when nothing is configured — the default, and a first-class state.
    pub fn local_only(&self) -> bool {
        self.ledger.is_none() && self.server.is_none()
    }
}

/// Reads the attachment out of `meta`.
///
/// A missing row is `None`, which is what makes an unconfigured store cost one
/// local `SELECT` and nothing else.
pub fn load(conn: &Connection) -> Result<Remotes> {
    Ok(Remotes {
        ledger: read_key(conn, KEY_LEDGER)?,
        server: read_key(conn, KEY_SERVER)?,
    })
}

fn read_key(conn: &Connection, key: &str) -> Result<Option<String>> {
    let v: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?;
    Ok(v.filter(|s| !s.is_empty()))
}

/// The recorded front door, read without opening the store the ordinary way.
///
/// This is the third precedence tier behind `--server` and `QP_SERVER`, and it
/// runs on every invocation of a served verb that named neither, so what it
/// costs is the whole of what a recorded default costs. Hence the shape:
/// a `stat`, a read-only `sqlite3_open`, one primary-key lookup on `meta`, and
/// a close. It deliberately does **not** go through [`crate::db::open`], which
/// runs the migration probes — a resolution step that migrated the store before
/// deciding whether to talk to it at all would pay the expensive half of the
/// thing it exists to avoid.
///
/// A path that is not there is `None` rather than an error: `resolve_path`
/// answers "where would the store be" even on a machine that has none, and the
/// answer to "is there a front door recorded there" is then no.
///
/// Every other failure propagates. Swallowing them would be the exact
/// silent-wrong-store shape the attach check exists to prevent, from the
/// opposite direction: an unreadable `meta` would resolve to "no front door",
/// the command would then open the same file read-write and usually succeed,
/// and a configured machine would answer from the local store while looking
/// configured to `qp remote doctor`.
pub fn recorded_server(db_path: &Path) -> Result<Option<String>> {
    if !db_path.exists() {
        return Ok(None);
    }
    // Read-only, and deliberately not `OpenFlags::default()`: that carries
    // `SQLITE_OPEN_URI`, under which a store path spelled like a URI would be
    // reinterpreted as one. Resolution must address the same bytes on disk that
    // `db::open` is about to.
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    read_key(&conn, KEY_SERVER)
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// Rejects a front-door url that is not an http(s) url this client would call.
///
/// The second layer for the server, sitting BESIDE `validate_ledger_url`'s
/// rather than loosening the shallow check both share. It delegates the shape
/// half to `client::check_url` so there is exactly one opinion in the tree
/// about when plaintext is acceptable — a url that stores clean here and is
/// then refused at call time would be a diagnostic that lies.
pub fn validate_server_url(url: &str) -> Result<()> {
    validate_url(url)?;
    crate::client::check_url(url)?;
    Ok(())
}

/// What a front door's health probe said about the process behind it.
///
/// Three variants because three different things go wrong and they want three
/// different fixes: nothing is running, something else is running, or quipu is
/// running and does not hold the store you asked about. Collapsing the first
/// two into "could not reach it" is the confidently-wrong answer a diagnostic
/// exists to end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerReach {
    /// A quipu front door answered, and said what it is and what it serves.
    Alive {
        version: Option<String>,
        projects: Vec<String>,
    },
    /// Something answered on that url and it is not a quipu front door.
    NotQuipu(String),
    /// Nothing answered.
    Unreachable(String),
}

impl ServerReach {
    /// The coarse form `qp remote show --check` prints, for symmetry with the ledger.
    pub fn reach(&self) -> Reach {
        match self {
            ServerReach::Alive { .. } => Reach::Ok,
            ServerReach::NotQuipu(d) | ServerReach::Unreachable(d) => Reach::Fault(d.clone()),
        }
    }
}

/// Asks a front door whether it is alive and which projects it serves.
///
/// `GET /healthz`, and nothing else. The health endpoint answers from
/// process-local state and deliberately refuses a project parameter, so the
/// probe cannot ask "do you serve X" — it asks what the process opened at
/// startup and compares locally. Asking the server instead would be asking it
/// to run a liveness check whose answer depends on user input, which is the
/// thing the process contract rules out.
pub fn probe_server(url: &str) -> ServerReach {
    let body = match crate::client::health(url) {
        Ok(v) => v,
        Err(e) => {
            let transport = e
                .downcast_ref::<crate::db::QuipuError>()
                .is_some_and(|q| q.kind() == "unavailable");
            let detail = format!("{e:#}");
            return match transport {
                true => ServerReach::Unreachable(detail),
                false => ServerReach::NotQuipu(detail),
            };
        }
    };
    // A quipu front door always reports the set it opened at startup, and that
    // set is never empty. Anything else answering 200 on this path is some
    // other service, and saying so is more useful than reporting no projects.
    let Some(projects) = body.get("projects").and_then(serde_json::Value::as_array) else {
        return ServerReach::NotQuipu(format!(
            "answered {} with a body that is not a quipu health payload: it has no `projects` list",
            crate::wire::HEALTH_PATH
        ));
    };
    ServerReach::Alive {
        version: body
            .get("version")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        projects: projects
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect(),
    }
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

    /// The server's shape check sits BESIDE the ledger's, over the same shallow one.
    ///
    /// Both halves matter. A url that would be read as a flag is refused
    /// before anything parses a host out of it, and each kind of url still
    /// refuses the other kind — a diagnostic that accepted an `s3://` url as a
    /// front door would then report the front door as unreachable, which is
    /// the confident wrong answer in a new costume.
    #[test]
    fn a_server_url_must_pass_the_shallow_check_and_then_be_an_http_url() {
        assert!(validate_server_url("--upload-pack=evil").is_err());
        assert!(validate_server_url(" https://qp.example.com").is_err());
        assert!(validate_server_url("s3://host.invalid/bucket").is_err());
        assert!(validate_server_url("https://qp.example.com").is_ok());
        assert!(validate_server_url("http://127.0.0.1:8787").is_ok());
        // The plaintext rule is `client::check_url`'s and is not re-litigated here.
        assert!(validate_server_url("http://qp.example.com").is_err());
        // And the ledger still refuses a front door.
        assert!(validate_ledger_url("https://qp.example.com").is_err());
    }

    #[test]
    fn local_only_needs_both_urls_absent() {
        let mut r = Remotes::default();
        assert!(r.local_only());
        r.server = Some("https://qp.example.com".to_string());
        assert!(!r.local_only(), "a server-only store is not local-only");
    }
}
