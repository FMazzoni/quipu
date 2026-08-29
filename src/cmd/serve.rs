//! `qp serve` — the front door: N existing stores behind one HTTP listener.
//!
#![doc = include_str!("../../docs/modules/serve.md")]

use crate::{db, registry, route, wire};
use anyhow::{Context, Result};
use clap::Args;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
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

/// How long a worker waits for a request body before abandoning the socket.
///
/// It is two numbers at once: the longest a legitimate slow client may take to
/// deliver a body, and the longest a stalled one may cost a worker. The second
/// is what keeps it small. A body is a handful of short strings that the client
/// wrote in the same call as the headers, so it is already in flight when a
/// worker first sees the request; five seconds is `wait`'s whole poll period
/// ten times over, and carries the 1 MB ceiling at 200 KB/s. Against that, four
/// simultaneously stalled clients cost one five-second hiccup that heals itself
/// rather than a front door that never answers again.
const READ_DEADLINE: Duration = Duration::from_secs(5);

/// How long a worker waits for its response to reach the socket.
///
/// Longer than [`READ_DEADLINE`] because the failure it bounds is rarer and the
/// legitimate case is larger: a kernel send buffer that autotunes to megabytes
/// swallows every ordinary payload without blocking at all, so a write that is
/// still going after thirty seconds means the peer stopped reading. The served
/// read verbs — `list`, `report`, `timeline`, `tree` — are why this exists at
/// all; a write verb's payload could never reach the buffer's ceiling.
const WRITE_DEADLINE: Duration = Duration::from_secs(30);

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
///
/// `key` is the store's `project_key`, read once when the store is opened at
/// startup, which is what makes the identity check a string compare rather than
/// a query per request. `None` only for a store that has none — unreachable for
/// a store this binary has opened, because `project::backfill` runs inside
/// `db::migrate` — and still handled rather than unwrapped, because "the served
/// store has no identity" has to be a refusal and never a panic in a process
/// that is meant to run for weeks.
struct Project {
    name: String,
    path: PathBuf,
    key: Option<String>,
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

    let mut projects = resolve(db_path, names)?;
    // Everything the run needs is acquired before the first accept. A process
    // that binds and *then* discovers it cannot open a store looks healthy to a
    // supervisor forever while answering `no_store` to every request. The
    // project key is read here, from the connection that proves the store
    // openable, and never again: an identity that is fixed before the listener
    // binds cannot drift, and the per-request check costs a string compare.
    for p in &mut projects {
        let conn = db::open(&p.path).with_context(|| format!("opening project `{}`", p.name))?;
        p.key = crate::project::read(&conn)?;
    }
    let projects = projects;

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
                .map(|p| serde_json::json!({
                    "name": p.name,
                    "path": p.path.display().to_string(),
                    "project_key": p.key,
                }))
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
    fn store(&self, name: Option<&str>, asserted_key: Option<&str>) -> Result<&PathBuf> {
        let p = match name {
            Some(n) => self.projects.iter().find(|p| p.name == n).ok_or_else(|| {
                db::no_store(format!("this server does not serve project `{n}`"), None)
            })?,
            None if self.projects.len() == 1 => &self.projects[0],
            None => {
                return Err(db::invalid_input(format!(
                    "this server serves {} projects; name one with `--project`",
                    self.projects.len()
                )))
            }
        };
        check_identity(p, asserted_key)?;
        Ok(&p.path)
    }
}

/// The served twin of `db::init`'s `project_key_mismatch`, at the socket.
///
/// A name got the caller to a store; this asks whether it is the store they
/// meant. A project name is machine-local — a registry maps a name to a path
/// per machine — so two hosts each serving their own `alpha` is the ordinary
/// case, and a front door that publishes only a name cannot be told apart from
/// an impostor by anything the caller can see.
///
/// It is `no_store`, exit 6, and deliberately neither `not_found` nor
/// `conflict`: exit 2 means "look again, it may change", and this never
/// changes. The store set is fixed before the listener binds, so no retry and
/// no wait can make this front door start serving the caller's project — which
/// is the same thing exit 6 already says for a project this process never
/// opened at all. The local twin is a `conflict` because there it genuinely is
/// a state conflict — `qp init` refusing to re-home a store — and here nothing
/// is being changed by anyone.
///
/// A caller that asserted nothing is not checked. The guard belongs to a client
/// that has a store of its own to be wrong about; see
/// `main::asserted_project_key` for why a storeless one cannot participate.
fn check_identity(p: &Project, asserted: Option<&str>) -> Result<()> {
    let Some(want) = asserted else { return Ok(()) };
    let name = &p.name;
    match p.key.as_deref() {
        Some(have) if have == want => Ok(()),
        Some(have) => Err(db::no_store(
            format!(
                "this front door's `{name}` is a different project: the store it serves \
                 belongs to project `{have}`, and the caller belongs to `{want}`. A project \
                 name is machine-local, so two hosts can each serve a project named \
                 `{name}` and they are not the same project. Point at the front door \
                 that serves your project, or name this one explicitly with \
                 `--project {name}` if you meant to address a project other than your own."
            ),
            None,
        )),
        None => Err(db::no_store(
            format!(
                "this front door's `{name}` has no project identity to compare against, so \
                 it cannot be shown to be the `{name}` the caller means. Run `qp init` in \
                 that store, on the machine that holds it."
            ),
            None,
        )),
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
            key: None,
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
            key: None,
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

/// Runs one request: a socket thread holds the connection, this holds the store.
///
/// The split is forced by a fact about blocking sockets: a `read` cannot be
/// interrupted by the thread that wants to give up on it. So the thread that
/// applies the deadline can never be the thread that is inside the read, and
/// the connection moves to a thread of its own for its whole life — body in,
/// response out — while the worker keeps the store work and the deadlines.
///
/// What that buys is the invariant the worker count rests on: [`WORKERS`]
/// still bounds the number of requests touching a store at once, because the
/// socket thread never opens one. What it costs is one thread per request, and
/// an abandoned socket thread outliving its worker.
fn handle(rq: tiny_http::Request, state: &State) {
    let began = Instant::now();
    let (msg_tx, msg_rx) = mpsc::channel();
    let (reply_tx, reply_rx) = mpsc::channel();
    std::thread::spawn(move || socket_thread(rq, &msg_tx, &reply_rx));

    let Ok(FromSocket::Head { method, url }) = msg_rx.recv() else {
        return;
    };
    let body = match msg_rx.recv_timeout(READ_DEADLINE) {
        Ok(FromSocket::Body(Ok(b))) => Ok(b),
        Ok(FromSocket::Body(Err(e))) => {
            Err(db::invalid_input(format!("unreadable request body: {e}")))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // The worker is now free; the socket thread is not, and answers for
            // itself if the body ever turns up. This line is the only record
            // that the request existed, because it never became a transaction.
            log(
                "warn",
                "read_timeout",
                serde_json::json!({
                    "method": method,
                    "url": url,
                    "ms": began.elapsed().as_millis(),
                    "deadline_s": READ_DEADLINE.as_secs(),
                }),
            );
            return;
        }
        // The socket thread stops early only when the connection did, and it
        // has already written whatever that leaves possible.
        Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
    };

    let (path, query) = split_once_or(&url, '?');
    let computed = if path == wire::HEALTH_PATH {
        // Process-local state only. A probe that opens a store runs migrations,
        // which is a probe that changes the thing it measures.
        Ok(health(state))
    } else {
        body.and_then(|b| answer(state, &method, path, query, &b))
    };
    let (status, payload, kind) = match computed {
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

    let bytes = serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec());
    if reply_tx.send((status, bytes)).is_err() {
        // The socket thread lost its race with this worker's own deadline and
        // has already written the refusal. Logging here would claim a status
        // that never reached anyone.
        return;
    }
    let write_err = match msg_rx.recv_timeout(WRITE_DEADLINE) {
        Ok(FromSocket::Wrote(e)) => e,
        // A client that stopped reading holds the socket thread exactly as a
        // stalled body does, and is released the same way. The store work is
        // already committed, so this is the process contract's restart window:
        // the caller has to treat an unanswered request as "may have landed".
        Err(mpsc::RecvTimeoutError::Timeout) => Some(format!(
            "response not written within {}s",
            WRITE_DEADLINE.as_secs()
        )),
        Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => {
            Some("connection closed before the response was written".to_string())
        }
    };
    log_request(state, &method, &url, status, kind, write_err, began);
}

/// The `/healthz` payload, all of it in memory since startup put it there.
fn health(state: &State) -> serde_json::Value {
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "bind": state.bound,
        "uptime_s": state.started.elapsed().as_secs(),
        "projects": state.projects.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
        // Additive beside `projects` rather than replacing it: `projects` is
        // what `remote::probe_server` reads to decide this is a quipu front
        // door at all, and rewriting it into objects would make every existing
        // consumer of this payload wrong at once. A name answers "is my project
        // here"; only a key answers "is it MY project", which is why
        // `qp remote doctor` compares the second — the first passes against an
        // impostor.
        "project_keys": state.projects.iter()
            .filter_map(|p| p.key.clone().map(|k| (p.name.clone(), serde_json::json!(k))))
            .collect::<serde_json::Map<_, _>>(),
    })
}

/// The request, from method check to payload. Every failure is a typed error.
fn answer(
    state: &State,
    method: &str,
    path: &str,
    query: &str,
    buf: &str,
) -> Result<serde_json::Value> {
    // `invalid_input` (400, exit 1) and deliberately not `not_found` (404, exit
    // 2). Run the wire contract's own decision procedure: could a different
    // request *body* have worked? No — the path is wrong, and the body was
    // never read. Is the thing addressed absent? That question is about a store
    // entity, and this is a URL. So it is neither retryable nor a statement
    // about the store, and 400 is the only row left.
    //
    // The cost of getting it wrong was concrete: exit 2 is, by the README's own
    // table, "wrong state / lost race — retry may succeed", and the wire
    // contract encourages retrying on it. A skill following the documentation
    // therefore looped forever against a base url that could never work.
    let Some(verb) = path.strip_prefix(wire::API_PREFIX) else {
        return Err(db::invalid_input(format!(
            "no endpoint at `{path}`; a quipu front door answers `{p}<verb>`. \
             If the url names a subpath, whatever is in front of this process \
             has to strip it before forwarding.",
            p = wire::API_PREFIX
        )));
    };
    let Some(handler) = route::lookup(verb) else {
        return Err(db::invalid_input(format!(
            "`{verb}` is not a verb this front door serves; this server is \
             quipu {v}",
            v = env!("CARGO_PKG_VERSION")
        )));
    };
    if method != "POST" {
        return Err(db::invalid_input(format!(
            "`{path}` takes POST, not {method}"
        )));
    }
    let store = state
        .store(
            query_param(query, "project").as_deref(),
            query_param(query, "project_key").as_deref(),
        )?
        .clone();
    if buf.len() > MAX_BODY {
        return Err(db::invalid_input(format!(
            "request body exceeds {MAX_BODY} bytes"
        )));
    }
    let body: serde_json::Value = if buf.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(buf)
            .map_err(|e| db::invalid_input(format!("request body is not JSON: {e}")))?
    };
    handler(&store, body)
}

/// What a worker tells its socket thread, and what it hears back.
///
/// `Head` is sent before the body is read so that a request abandoned at the
/// read deadline can still be logged as the request it was; without it the
/// only account of a timed-out request would be a count.
enum FromSocket {
    Head { method: String, url: String },
    Body(std::result::Result<String, String>),
    Wrote(Option<String>),
}

/// Status and bytes, handed back for the socket thread to write.
type Reply = (u16, Vec<u8>);

/// Owns one connection for its whole life: body in, response out.
///
/// It never touches a store, which is what keeps [`WORKERS`] meaning "requests
/// in flight against SQLite" after this thread was introduced.
///
/// Both places where the worker has gone converge on the same refusal rather
/// than on a drop: `tiny_http` answers an unanswered request with a bare 500,
/// and a slow client meeting a 500 would read a bug in the server where there
/// was a bug in its own link. `invalid_input` is the same kind an unreadable
/// body already produced, and it says the true thing — the body promised by
/// `Content-Length` was not delivered.
fn socket_thread(
    mut rq: tiny_http::Request,
    tx: &mpsc::Sender<FromSocket>,
    rx: &mpsc::Receiver<Reply>,
) {
    let head = FromSocket::Head {
        method: rq.method().as_str().to_string(),
        url: rq.url().to_string(),
    };
    if tx.send(head).is_err() {
        return;
    }
    let mut buf = String::new();
    let body = rq
        .as_reader()
        .take(MAX_BODY as u64 + 1)
        .read_to_string(&mut buf)
        .map(|_| buf)
        .map_err(|e| e.to_string());
    if tx.send(FromSocket::Body(body)).is_ok() {
        if let Ok((status, bytes)) = rx.recv() {
            let err = write_response(rq, status, bytes)
                .err()
                .map(|e| e.to_string());
            let _ = tx.send(FromSocket::Wrote(err));
            return;
        }
    }
    let (status, body) = wire::failure(&db::invalid_input(format!(
        "request body did not arrive within {}s",
        READ_DEADLINE.as_secs()
    )));
    let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    let _ = write_response(rq, status, bytes);
}

/// The one place a response reaches a socket.
///
/// A write failure here is the client having gone away mid-response. It is an
/// ordinary `EPIPE` because `ignore_sigpipe` ran at startup, so it costs this
/// request and nothing else.
fn write_response(
    rq: tiny_http::Request,
    status: u16,
    bytes: Vec<u8>,
) -> std::result::Result<(), std::io::Error> {
    let ct = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("static header");
    // Names the process that wrote this, on every response including the
    // failures. It is what lets a client tell "quipu answered" from "something
    // answered": a service that returns `200 {}` on every path used to be
    // accepted, and every read then rendered as an empty board at exit 0.
    let version = tiny_http::Header::from_bytes(
        wire::VERSION_HEADER.as_bytes(),
        env!("CARGO_PKG_VERSION").as_bytes(),
    )
    .expect("static header");
    rq.respond(
        tiny_http::Response::from_data(bytes)
            .with_status_code(status)
            .with_header(ct)
            .with_header(version),
    )
}

/// Logs what the store could not have recorded about one answered request.
fn log_request(
    state: &State,
    method: &str,
    url: &str,
    status: u16,
    kind: Option<String>,
    write_err: Option<String>,
    began: Instant,
) {
    let mut fields = serde_json::json!({
        "method": method, "url": url, "status": status, "ms": began.elapsed().as_millis(),
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

/// One addressing parameter out of the query string.
///
/// Addressing, never arguments: `project` says which store, `project_key` says
/// which project that store had better be. Both stay out of the request body
/// because every served args struct sets `deny_unknown_fields`, and because
/// neither is a thing the verb does.
fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == name).then(|| percent_decode(v))
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
        let q = |s, k| query_param(s, k);
        assert_eq!(q("project=alpha", "project").as_deref(), Some("alpha"));
        assert_eq!(q("a=1&project=b%20c", "project").as_deref(), Some("b c"));
        assert_eq!(q("", "project"), None);
        assert_eq!(q("other=x", "project"), None);
        assert_eq!(
            q("project=alpha&project_key=k1", "project_key").as_deref(),
            Some("k1")
        );
        assert_eq!(q("project=alpha", "project_key"), None);
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
                key: Some("key-a".into()),
            }],
            bound: "127.0.0.1:0".into(),
            started: Instant::now(),
            access_log: false,
        };
        let e = state.store(Some("b"), None).unwrap_err();
        let q = e.downcast_ref::<db::QuipuError>().expect("typed");
        assert_eq!(q.kind(), "no_store");
        assert_eq!(q.exit_code(), 6);
        assert_eq!(state.store(None, None).unwrap(), &PathBuf::from("/x"));
    }

    /// A name reaches a store; only the key says it is the right project.
    ///
    /// The same verdict as an unserved project — `no_store`, exit 6 — because
    /// the answer is the same: this front door is not attached to the project
    /// the caller means, and no retry can change that.
    #[test]
    fn a_name_that_matches_a_different_project_is_no_store() {
        let state = State {
            projects: vec![Project {
                name: "alpha".into(),
                path: PathBuf::from("/x"),
                key: Some("key-mine".into()),
            }],
            bound: "127.0.0.1:0".into(),
            started: Instant::now(),
            access_log: false,
        };
        // The impostor: same name, different project.
        let e = state.store(Some("alpha"), Some("key-theirs")).unwrap_err();
        let q = e.downcast_ref::<db::QuipuError>().expect("typed");
        assert_eq!(q.kind(), "no_store");
        assert_eq!(q.exit_code(), 6);
        // The same project by name, and a caller that asserts nothing.
        assert!(state.store(Some("alpha"), Some("key-mine")).is_ok());
        assert!(state.store(Some("alpha"), None).is_ok());
        // The sole-store path is guarded too: naming nothing is not checking
        // nothing, and a client with a recorded front door names nothing.
        assert!(state.store(None, Some("key-theirs")).is_err());
    }
}
