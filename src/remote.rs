//! Remote attachment: the two optional urls in `meta`, and the git/ssh env quipu spawns with.
//!
#![doc = include_str!("../docs/modules/remote_config.md")]

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};

/// `meta` key holding the claim authority url.
pub const KEY_AUTHORITY: &str = "remote.authority";

/// `meta` key holding the ledger sync url.
pub const KEY_LEDGER: &str = "remote.ledger";

/// How long a shared ssh master lingers after its last channel closes, in seconds.
const CONTROL_PERSIST_SECS: u32 = 60;

/// Ceiling on a `ControlPath`, below the 108-byte `sun_path` limit on Linux.
///
/// Over it, quipu drops multiplexing rather than handing ssh a path it will
/// truncate — see `docs/modules/remote.md`.
const MAX_CONTROL_PATH: usize = 96;

/// The two urls a store may be attached to. `None` on both is the local-only state.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Remotes {
    pub authority: Option<String>,
    pub ledger: Option<String>,
}

impl Remotes {
    /// True when nothing is configured — the default, and a first-class state.
    pub fn local_only(&self) -> bool {
        self.authority.is_none() && self.ledger.is_none()
    }
}

/// Reads both keys out of `meta`.
///
/// A missing row is `None`, which is what makes an unconfigured store cost one
/// local `SELECT` and nothing else.
pub fn load(conn: &Connection) -> Result<Remotes> {
    Ok(Remotes {
        authority: read_key(conn, KEY_AUTHORITY)?,
        ledger: read_key(conn, KEY_LEDGER)?,
    })
}

fn read_key(conn: &Connection, key: &str) -> Result<Option<String>> {
    let v: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?;
    Ok(v.filter(|s| !s.is_empty()))
}

/// Reads just the authority, which is all `claim` needs.
///
/// One `SELECT`, deliberately: an unconfigured store must not pay two reads to
/// discover it has no remote, and the local claim path is measured in
/// milliseconds.
pub fn authority(conn: &Connection) -> Result<Option<String>> {
    read_key(conn, KEY_AUTHORITY)
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

/// Rejects a url that cannot survive being handed to `git` as an argument.
///
/// Deliberately shallow: quipu does not know what schemes a user's git
/// supports, and a scheme allow-list would reject working configurations. What
/// it does catch is the input that would silently misbehave — empty, padded
/// with whitespace, carrying a control character, or starting with `-` and so
/// parsed by git as a flag.
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
            "remote url must not start with '-' (git would read it as a flag)",
        ));
    }
    Ok(())
}

/// 64-bit FNV-1a. Names a control socket; not a security primitive.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

fn short_hash(s: &str) -> String {
    format!("{:08x}", (fnv1a(s) & 0xffff_ffff) as u32)
}

/// Whether a url reaches its far end over ssh, and so can be multiplexed.
///
/// `http(s)://`, `git://` and bare filesystem paths never spawn ssh, so
/// injecting a `GIT_SSH_COMMAND` for them buys nothing.
fn is_ssh_url(url: &str) -> bool {
    if url.starts_with("ssh://") {
        return true;
    }
    if url.contains("://") {
        return false;
    }
    // scp-like syntax: [user@]host:path, where the colon precedes no slash.
    match url.split_once(':') {
        Some((host, _)) => !host.is_empty() && !host.contains('/'),
        None => false,
    }
}

/// Directory holding quipu's ssh control sockets: `<store-dir>/ssh`.
///
/// Under quipu's own directory rather than `~/.ssh`, so a stale socket is
/// quipu's to clean up and never appears in the user's ssh config's blast
/// radius.
pub fn socket_dir(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("ssh")
}

/// The `GIT_SSH_COMMAND` quipu spawns git children with, or `None` to leave the environment alone.
///
/// `lane` is what keeps a fan-out off a single shared master; pass the acting
/// agent's id. The burst hazard and this resolution are written up in
/// `docs/modules/remote.md` — read that before changing the key.
pub fn ssh_command(db_path: &Path, url: &str, lane: &str) -> Option<String> {
    if !multiplexing_enabled() {
        return None;
    }
    if std::env::var_os("GIT_SSH_COMMAND").is_some() || std::env::var_os("GIT_SSH").is_some() {
        return None;
    }
    if !is_ssh_url(url) {
        return None;
    }
    let dir = socket_dir(db_path);
    let path = dir.join(format!("m-{}-{}", short_hash(url), short_hash(lane)));
    let path = path.to_str()?;
    if path.len() > MAX_CONTROL_PATH {
        return None;
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    Some(format!(
        "ssh -o ControlMaster=auto -o ControlPath={path} -o ControlPersist={CONTROL_PERSIST_SECS}"
    ))
}

/// `QUIPU_SSH_MULTIPLEX` set to `0`/`off`/`false`/`no` turns the injection off.
fn multiplexing_enabled() -> bool {
    match std::env::var("QUIPU_SSH_MULTIPLEX") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "false" | "no"
        ),
        Err(_) => true,
    }
}

/// A `git` invocation carrying quipu's ssh environment, ready for args.
///
/// Every git child quipu spawns against a remote must come from here; that is
/// the single place the multiplexing decision is made.
pub fn git(db_path: &Path, url: &str, lane: &str) -> std::process::Command {
    let mut c = std::process::Command::new("git");
    if let Some(ssh) = ssh_command(db_path, url, lane) {
        c.env("GIT_SSH_COMMAND", ssh);
    }
    // Never wait on an interactive credential or host-key prompt: a hung child
    // is indistinguishable from an unreachable host and neither is worth an
    // agent's session.
    c.env("GIT_TERMINAL_PROMPT", "0");
    c
}

/// Outcome of a reachability probe. Opt-in everywhere; nothing runs it implicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    Ok,
    Unreachable,
    /// `git` itself could not be spawned.
    NoGit,
}

/// Asks the far end for its refs. The only function in this module that touches the network.
///
/// `git ls-remote` exits 0 against a reachable remote — including a freshly
/// created empty bare repo, verified — and 128 when it cannot get there at all.
pub fn probe(db_path: &Path, url: &str, lane: &str) -> Reach {
    match git(db_path, url, lane)
        .args(["ls-remote", "--", url])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Reach::Ok,
        Ok(_) => Reach::Unreachable,
        Err(_) => Reach::NoGit,
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
    fn ssh_urls_are_recognised() {
        assert!(is_ssh_url("ssh://host.invalid/srv/authority.git"));
        assert!(is_ssh_url("user@host.invalid:authority.git"));
        assert!(!is_ssh_url("https://host.invalid/authority.git"));
        assert!(!is_ssh_url("git://host.invalid/authority.git"));
        assert!(!is_ssh_url("/srv/git/authority.git"));
        assert!(!is_ssh_url("./authority.git"));
    }

    /// Whether the ambient environment has already opted out of the injection.
    ///
    /// `ssh_command` bows out when the user already set one, so the two
    /// assertions below would be vacuous in an environment that has.
    fn user_ssh_env_is_set() -> bool {
        std::env::var_os("GIT_SSH_COMMAND").is_some()
            || std::env::var_os("GIT_SSH").is_some()
            || !multiplexing_enabled()
    }

    #[test]
    fn lane_changes_the_socket() {
        if user_ssh_env_is_set() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        let url = "user@host.invalid:authority.git";
        let a = ssh_command(&db, url, "agent-a").unwrap();
        let b = ssh_command(&db, url, "agent-b").unwrap();
        assert_ne!(a, b, "two lanes must not share one ControlMaster");
        assert!(a.contains("ControlMaster=auto"));
    }

    #[test]
    fn non_ssh_urls_get_no_injection() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db.sqlite");
        assert!(ssh_command(&db, "/srv/git/authority.git", "a").is_none());
    }

    #[test]
    fn validate_url_rejects_the_dangerous_shapes() {
        assert!(validate_url("").is_err());
        assert!(validate_url(" host.invalid:x.git").is_err());
        assert!(validate_url("--upload-pack=evil").is_err());
        assert!(validate_url("host.invalid:x\ny.git").is_err());
        assert!(validate_url("ssh://host.invalid/x.git").is_ok());
        assert!(validate_url("/srv/git/x.git").is_ok());
    }
}
