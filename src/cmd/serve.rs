//! `qp serve` — the front door: N existing stores behind one HTTP listener.
//!
#![doc = include_str!("../../docs/modules/serve.md")]

use crate::{db, registry, route, wire};
use anyhow::{Context, Result};
use clap::Args;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Worker threads that call `recv_timeout` on the shared listener.
///
/// Fixed rather than a flag: it is not a knob anyone has evidence to turn, and
/// every request is one short `BEGIN IMMEDIATE` transaction, so the useful
/// concurrency is bounded by the write lock rather than by this number.
const WORKERS: usize = 4;

/// How long a worker blocks before re-reading the stop flag.
const POLL: Duration = Duration::from_millis(200);

/// Ceiling on a request body.
///
/// A verb's arguments are a handful of short strings; anything larger is a
/// mistake or an attack, and it is refused before it is parsed.
const MAX_BODY: usize = 1024 * 1024;

/// Set by the signal handler and by nothing else.
///
/// A handler may only do async-signal-safe work, so it does exactly one thing:
/// stores `true` here. Every store close, join and log line happens on an
/// ordinary thread that observed it — closing a SQLite connection from a
/// signal context can deadlock against the transaction it interrupted.
static STOP: AtomicBool = AtomicBool::new(false);

/// Set when a worker's `recv` fails: the one runtime fault that ends the run.
///
/// A per-request failure is a response, never an exit — an operator watching
/// restart counts must be able to read a restart as "the process died".
static LISTENER_FAULT: AtomicBool = AtomicBool::new(false);

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Address to listen on. Loopback by default: `--as` is unvalidated free
    /// text, so anything that reaches the port can act as any agent.
    #[arg(long, default_value = "127.0.0.1:8787")]
    pub bind: String,
    /// Log one line per successful request too. Off by default: the store's
    /// event log is a better request log, and a poll loop per waiting agent is
    /// the fastest way to fill a disk this project has yet invented.
    #[arg(long)]
    pub access_log: bool,
}

/// One store this process serves, resolved and proven openable at startup.
struct Project {
    name: String,
    path: PathBuf,
}

/// Starts the front door and does not return until it is told to stop.
///
/// `names` is the global `--project` list; empty means "the store this
/// invocation would have used", which is what makes `qp serve` work from
/// inside a checkout with no registry entry at all.
pub fn run(db_path: &std::path::Path, a: ServeArgs, names: &[String]) -> Result<()> {
    // Before any thread exists and before the listener binds. See the module
    // page: this is the fix for the remote-kill hazard, and it is deliberately
    // here rather than in `main`.
    ignore_sigpipe();
    install_stop_handlers();

    let projects = resolve(db_path, names)?;
    // Everything the run needs is acquired before the first accept. A process
    // that binds and *then* discovers it cannot open a store looks healthy to a
    // supervisor forever while answering `no_store` to every request.
    for p in &projects {
        db::open(&p.path).with_context(|| format!("opening project `{}`", p.name))?;
    }

    let server = tiny_http::Server::http(&a.bind)
        .map_err(|e| db::invalid_input(format!("cannot listen on `{}`: {e}", a.bind)))?;
    let bound = server
        .server_addr()
        .to_ip()
        .map_or_else(|| a.bind.clone(), |s| s.to_string());
    let started = Instant::now();

    log(
        "info",
        "startup",
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "bind": bound,
            "projects": projects.iter()
                .map(|p| serde_json::json!({"name": p.name, "path": p.path.display().to_string()}))
                .collect::<Vec<_>>(),
        }),
    );

    let state = Arc::new(State {
        projects,
        bound: bound.clone(),
        started,
        access_log: a.access_log,
    });
    let server = Arc::new(server);

    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            let server = Arc::clone(&server);
            let state = Arc::clone(&state);
            std::thread::spawn(move || worker(&server, &state))
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    // Dropping the listener closes it; dropping every per-request connection
    // already happened when each request finished.
    drop(server);

    if LISTENER_FAULT.load(Ordering::Relaxed) {
        log(
            "error",
            "shutdown",
            serde_json::json!({"reason": "listener_fault", "exit": 1}),
        );
        std::process::exit(1);
    }
    log(
        "info",
        "shutdown",
        serde_json::json!({"reason": "signal", "exit": 0}),
    );
    Ok(())
}

/// Everything a request handler reads, all of it captured before the first accept.
struct State {
    projects: Vec<Project>,
    bound: String,
    started: Instant,
    access_log: bool,
}

impl State {
    /// The store a request named, or the sole store when it named none.
    ///
    /// A `--project` this process did not open can never appear at runtime,
    /// which is what makes `no_store` mean "this front door does not serve
    /// that project" rather than "something might be broken".
    fn store(&self, name: Option<&str>) -> Result<&PathBuf> {
        match name {
            Some(n) => self
                .projects
                .iter()
                .find(|p| p.name == n)
                .map(|p| &p.path)
                .ok_or_else(|| {
                    db::no_store(format!("this server does not serve project `{n}`"), None)
                }),
            None if self.projects.len() == 1 => Ok(&self.projects[0].path),
            None => Err(db::invalid_input(format!(
                "this server serves {} projects; name one with `--project`",
                self.projects.len()
            ))),
        }
    }
}

/// Resolves the store set, once, at startup.
fn resolve(db_path: &std::path::Path, names: &[String]) -> Result<Vec<Project>> {
    if names.is_empty() {
        let conn = db::open(db_path)?;
        let name = crate::project::read_name(&conn)?.unwrap_or_else(|| "default".to_string());
        return Ok(vec![Project {
            name,
            path: db_path.to_path_buf(),
        }]);
    }
    let reg = registry::load()?;
    let mut out = Vec::with_capacity(names.len());
    for n in names {
        let entry = reg.lookup(n).ok_or_else(|| {
            db::no_store(
                format!(
                    "project `{n}` is not in the registry at {}; \
                     run `qp project register` on the machine that holds its store",
                    reg.path.display()
                ),
                None,
            )
        })?;
        out.push(Project {
            name: entry.name.clone(),
            path: entry.path.clone(),
        });
    }
    Ok(out)
}

fn worker(server: &tiny_http::Server, state: &State) {
    while !STOP.load(Ordering::Relaxed) {
        match server.recv_timeout(POLL) {
            Ok(Some(rq)) => handle(rq, state),
            Ok(None) => {}
            Err(e) => {
                // The one runtime fault that is not a response. Everything else
                // — a malformed body, a lost race, a missing task — is an
                // envelope over the socket and the process keeps serving.
                log(
                    "error",
                    "listener_fault",
                    serde_json::json!({"detail": e.to_string()}),
                );
                LISTENER_FAULT.store(true, Ordering::Relaxed);
                STOP.store(true, Ordering::Relaxed);
                return;
            }
        }
    }
}

fn handle(mut rq: tiny_http::Request, state: &State) {
    let began = Instant::now();
    let method = rq.method().as_str().to_string();
    let raw_url = rq.url().to_string();
    let (path, query) = split_once_or(&raw_url, '?');

    if path == wire::HEALTH_PATH {
        // Process-local state only. A probe that opens a store runs migrations,
        // which is a probe that changes the thing it measures.
        let body = serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "bind": state.bound,
            "uptime_s": state.started.elapsed().as_secs(),
            "projects": state.projects.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
        });
        respond(rq, 200, &body, state, &method, &raw_url, None, began);
        return;
    }

    let (status, body, kind) = match answer(&mut rq, state, &method, path, query) {
        Ok(v) => (200, v, None),
        Err(e) => {
            let (s, b) = wire::failure(&e);
            let kind = b["error"]["kind"]
                .as_str()
                .unwrap_or("internal")
                .to_string();
            (s, b, Some(kind))
        }
    };
    respond(rq, status, &body, state, &method, &raw_url, kind, began);
}

/// The request, from method check to payload. Every failure is a typed error.
fn answer(
    rq: &mut tiny_http::Request,
    state: &State,
    method: &str,
    path: &str,
    query: &str,
) -> Result<serde_json::Value> {
    let Some(verb) = path.strip_prefix(wire::API_PREFIX) else {
        return Err(db::not_found(format!("no endpoint at `{path}`"), None));
    };
    let Some(handler) = route::lookup(verb) else {
        return Err(db::not_found(
            format!("`{verb}` is not a verb this front door serves"),
            None,
        ));
    };
    if method != "POST" {
        return Err(db::invalid_input(format!(
            "`{path}` takes POST, not {method}"
        )));
    }
    let store = state.store(project_param(query).as_deref())?.clone();
    let mut buf = String::new();
    rq.as_reader()
        .take(MAX_BODY as u64 + 1)
        .read_to_string(&mut buf)
        .map_err(|e| db::invalid_input(format!("unreadable request body: {e}")))?;
    if buf.len() > MAX_BODY {
        return Err(db::invalid_input(format!(
            "request body exceeds {MAX_BODY} bytes"
        )));
    }
    let body: serde_json::Value = if buf.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&buf)
            .map_err(|e| db::invalid_input(format!("request body is not JSON: {e}")))?
    };
    handler(&store, body)
}

/// Writes the response, then logs what the store could not have recorded.
///
/// A write failure here is the client having gone away mid-response. It is an
/// ordinary `EPIPE` because `ignore_sigpipe` ran at startup, so it costs this
/// request and nothing else.
#[allow(clippy::too_many_arguments)]
fn respond(
    rq: tiny_http::Request,
    status: u16,
    body: &serde_json::Value,
    state: &State,
    method: &str,
    url: &str,
    kind: Option<String>,
    began: Instant,
) {
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("static header");
    let resp = tiny_http::Response::from_data(bytes)
        .with_status_code(status)
        .with_header(header);
    let write_err = rq.respond(resp).err().map(|e| e.to_string());

    let ms = began.elapsed().as_millis();
    let mut fields = serde_json::json!({
        "method": method, "url": url, "status": status, "ms": ms,
    });
    if let Some(k) = &kind {
        fields["kind"] = serde_json::json!(k);
    }
    if let Some(e) = &write_err {
        fields["write_error"] = serde_json::json!(e);
    }
    // The rule that decides what goes on stderr is: log what SQLite does not
    // record. A refused request never became a transaction, a 500 is a bug by
    // definition, and a dropped response was never seen by anyone.
    let level = match status {
        s if s >= 500 => "error",
        s if s >= 400 => "warn",
        _ if write_err.is_some() => "warn",
        _ => "info",
    };
    if status >= 400 || write_err.is_some() || state.access_log {
        log(level, "request", fields);
    }
}

fn project_param(query: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == "project").then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn split_once_or(s: &str, sep: char) -> (&str, &str) {
    s.split_once(sep).unwrap_or((s, ""))
}

/// One JSON object per line on stderr, and no logging framework.
///
/// `serde_json` is already a dependency and a log line needs nothing else. The
/// level is a field in the object rather than a global filter with a subscriber
/// registry — filtering is `grep`'s job, or the log system's, after the line is
/// written. Rotation, retention and routing belong to whatever supervises this
/// process; quipu writes to stderr and stops there.
fn log(level: &str, event: &str, mut fields: serde_json::Value) {
    let obj = fields.as_object_mut().expect("log fields are an object");
    obj.insert("ts".into(), serde_json::json!(crate::time::now_rfc3339()));
    obj.insert("level".into(), serde_json::json!(level));
    obj.insert("event".into(), serde_json::json!(event));
    eprintln!("{}", serde_json::Value::Object(obj.clone()));
}

/// Narrows `SIGPIPE` to `SIG_IGN` for the life of this process.
///
/// `main` restores `SIG_DFL` so `qp list | head -1` dies like a Unix filter,
/// which is right for a CLI and a remote kill for a server: with the default
/// disposition, a write to a socket whose peer has gone away delivers `SIGPIPE`
/// and takes down a process that is serving everyone else. Ignoring it turns
/// that into an ordinary `EPIPE` on the one request that lost its reader.
///
/// `SIG_IGN` rather than `MSG_NOSIGNAL` on each write: the socket writes belong
/// to `tiny_http`, so there is no seam to pass a flag through without forking
/// the crate, and a per-call-site fix is the failure mode the CLI's own
/// disposition change was chosen to avoid. This is one call that covers every
/// present and future write path in the serving process.
///
/// Safety: setting a disposition to `SIG_IGN` is always well-defined, and this
/// runs before any worker thread exists.
#[cfg(unix)]
fn ignore_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

#[cfg(not(unix))]
fn ignore_sigpipe() {}

/// `SIGTERM` and `SIGINT` mean stop, and mean nothing else.
///
/// Safety: `signal` installs a handler that does one relaxed atomic store,
/// which is async-signal-safe; the installation itself runs single-threaded at
/// startup.
#[cfg(unix)]
fn install_stop_handlers() {
    extern "C" fn on_stop(_sig: libc::c_int) {
        STOP.store(true, Ordering::Relaxed);
    }
    unsafe {
        libc::signal(libc::SIGTERM, on_stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_stop as *const () as libc::sighandler_t);
    }
}

#[cfg(not(unix))]
fn install_stop_handlers() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_project_query_parameter_is_decoded() {
        assert_eq!(project_param("project=alpha").as_deref(), Some("alpha"));
        assert_eq!(project_param("a=1&project=b%20c").as_deref(), Some("b c"));
        assert_eq!(project_param(""), None);
        assert_eq!(project_param("other=x"), None);
    }

    /// Naming a project this process did not open is `no_store`, not `not_found`.
    ///
    /// The distinction is the exit code: 6 says "this machine is not attached
    /// to that project", which a caller must never retry, and 2 says "the store
    /// looked and did not have it", which it may.
    #[test]
    fn an_unserved_project_is_no_store() {
        let state = State {
            projects: vec![Project {
                name: "a".into(),
                path: PathBuf::from("/x"),
            }],
            bound: "127.0.0.1:0".into(),
            started: Instant::now(),
            access_log: false,
        };
        let e = state.store(Some("b")).unwrap_err();
        let q = e.downcast_ref::<db::QuipuError>().expect("typed");
        assert_eq!(q.kind(), "no_store");
        assert_eq!(q.exit_code(), 6);
        assert_eq!(state.store(None).unwrap(), &PathBuf::from("/x"));
    }
}
