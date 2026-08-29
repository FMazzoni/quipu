//! End-to-end tests for the HTTP front door: every served verb, over a real
//! socket.
//!
//! Every server here binds an ephemeral loopback port and is torn down with the
//! test. Nothing reaches a network.
//!
//! The properties, in the order they matter:
//!
//! 1. **The exit code is unchanged from the local case.** Every failing command
//!    is run twice — once against a local store, once through the front door —
//!    and the two exit codes and the two `kind` strings must agree. That is the
//!    whole point of the wire contract: an existing skill's exit-code branch
//!    keeps working with a server in the path.
//! 2. **The envelope survives.** Success is the bare payload, failure is
//!    `{"error": ...}`, and the status is the documented projection of `kind`.
//! 3. **`_effects` crosses the socket, in both directions of travel.**
//!    `complete` and `cancel` promote dependents through the readiness sweep;
//!    `block` freezes a container's subtree and demotes it. The caller never
//!    named any of them and has no other way to be told.
//! 4. **A lost race is a lost race.** Several client *processes* against one
//!    ticket, through the server, produce exactly one winner — because
//!    `BEGIN IMMEDIATE` is the guarantee and the server changes nothing about
//!    it. The fixture is minted through the served `add`, which is why `add` is
//!    in this slice: a client's local `add` would file into the wrong store.
//! 5. **A client that leaves mid-response does not kill the process.** With
//!    `SIGPIPE` at its CLI disposition that write is a remote shutdown of a
//!    process serving everyone else.
//! 6. **An empty queue is not a failure.** `take` chooses, and choosing nothing
//!    is exit 0 with a null payload and a 200 on the status line. A poller that
//!    read "no work" as "something broke" would retry forever.

use assert_cmd::Command;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};

fn qp_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin("qp")
}

/// A store with nothing in it, in its own directory.
fn store(dir: &Path) -> PathBuf {
    let db = dir.join(".quipu").join("db.sqlite");
    Command::cargo_bin("qp")
        .unwrap()
        .env("QP_DB", &db)
        .args(["init", "--prefix", "QP"])
        .assert()
        .success();
    db
}

/// A `qp serve` child, its port, and everything it wrote to stderr.
///
/// stderr is drained by a thread rather than read on demand: a full pipe blocks
/// the process being tested, which would turn a logging change into a hang.
struct Server {
    child: Child,
    url: String,
    log: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start(db: &Path) -> Self {
        Self::start_with(db, &[])
    }

    fn start_with(db: &Path, extra: &[&str]) -> Self {
        let mut child = std::process::Command::new(qp_bin())
            .env("QP_DB", db)
            .args(["serve", "--bind", "127.0.0.1:0"])
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn qp serve");
        let mut reader = BufReader::new(child.stderr.take().expect("piped stderr"));
        // `real_main`'s project-mismatch warning is prose and can precede the
        // first structured line, so skip to the startup object rather than
        // asserting on line one.
        let mut seen: Vec<String> = Vec::new();
        let bind = loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).expect("read stderr");
            assert!(n > 0, "qp serve exited before logging startup: {seen:?}");
            let trimmed = line.trim_end().to_string();
            seen.push(trimmed.clone());
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&trimmed) {
                if v["event"] == "startup" {
                    break v["bind"].as_str().expect("bind address").to_string();
                }
            }
        };

        let log = Arc::new(Mutex::new(seen));
        let sink = Arc::clone(&log);
        std::thread::spawn(move || {
            for line in reader.lines().map_while(Result::ok) {
                sink.lock().unwrap().push(line);
            }
        });
        Server {
            child,
            url: format!("http://{bind}"),
            log,
        }
    }

    fn port(&self) -> u16 {
        self.url.rsplit(':').next().unwrap().parse().unwrap()
    }

    fn lines(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One `qp` invocation's observable contract: exit code, payload, error kind.
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn payload(&self) -> serde_json::Value {
        serde_json::from_str(self.stdout.trim())
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {:?}", self.stdout))
    }
    /// The `kind` from the error envelope on stderr.
    ///
    /// Scans for the line that *carries* an envelope rather than taking the
    /// first JSON line: under `--json` the project-mismatch warning is also
    /// JSON Lines, and it comes first.
    fn kind(&self) -> Option<String> {
        self.stderr.lines().find_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            Some(v["error"]["kind"].as_str()?.to_string())
        })
    }
}

/// Runs `qp <args> --json` against a local store.
fn local(db: &Path, args: &[&str]) -> Run {
    run(std::process::Command::new(qp_bin()).env("QP_DB", db), args)
}

/// Runs `qp <args> --json` through the front door, with no local store in play.
///
/// `QP_DB` is pointed at a path that does not exist on purpose: if the client
/// ever fell back to a local store, every assertion below would pass against
/// the wrong file and prove nothing.
fn remote(srv: &Server, args: &[&str]) -> Run {
    let mut c = std::process::Command::new(qp_bin());
    c.env("QP_SERVER", &srv.url)
        .env("QP_DB", "/nonexistent/quipu/db.sqlite");
    run(&mut c, args)
}

/// `--json` is appended unless the caller already placed it.
///
/// `qp tag` declares its flag on the *parent* args, so `tag QP-1 add x --json`
/// is a parse error — the trailing flag lands on the subcommand. Callers that
/// need it earlier pass it themselves.
fn run(c: &mut std::process::Command, args: &[&str]) -> Run {
    let mut argv: Vec<&str> = args.to_vec();
    if !argv.contains(&"--json") {
        argv.push("--json");
    }
    let out = c.args(&argv).output().expect("run qp");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// One raw HTTP POST, so the status line itself can be asserted.
///
/// Hand-written rather than through a client crate: the status is what is under
/// test, and a client that maps it to an error type hides the thing being
/// checked. `Connection: close` makes the read terminate at EOF.
fn post(port: u16, path: &str, body: &str) -> (u16, String) {
    request(port, &format!("POST {path} HTTP/1.1"), Some(body))
}

fn request(port: u16, line: &str, body: Option<&str>) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let body = body.unwrap_or("");
    let req = format!(
        "{line}\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).expect("write request");
    let mut raw = String::new();
    s.read_to_string(&mut raw).expect("read response");
    let status: u16 = raw
        .split_whitespace()
        .nth(1)
        .expect("status line")
        .parse()
        .expect("status code");
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

// ---------------------------------------------------------------------------
// the four verbs
// ---------------------------------------------------------------------------

/// add → assign → claim → complete, end to end, over a socket.
#[test]
fn the_four_verbs_round_trip_through_the_front_door() {
    let tmp = tempfile::tempdir().unwrap();
    let srv = Server::start(&store(tmp.path()));

    let created = remote(&srv, &["add", "wire the front door"]);
    assert_eq!(created.code, 0, "add failed: {}", created.stderr);
    assert_eq!(created.payload()["display_id"], "QP-1");
    assert_eq!(created.payload()["state"], "ready");

    let assigned = remote(&srv, &["assign", "QP-1", "--to", "w-alpha"]);
    assert_eq!(assigned.code, 0, "assign failed: {}", assigned.stderr);
    assert_eq!(assigned.payload()["agent_id"], "w-alpha");
    assert_eq!(assigned.payload()["state"], "assigned");

    let claimed = remote(&srv, &["claim", "QP-1", "--as", "w-alpha"]);
    assert_eq!(claimed.code, 0, "claim failed: {}", claimed.stderr);
    assert_eq!(claimed.payload()["state"], "running");
    assert!(
        claimed.payload()["expires_at"].is_string(),
        "the lease must survive the wire; it is what `reclaim` reads"
    );

    let done = remote(
        &srv,
        &[
            "complete",
            "QP-1",
            "--as",
            "w-alpha",
            "--decision",
            "shipped",
        ],
    );
    assert_eq!(done.code, 0, "complete failed: {}", done.stderr);
    assert_eq!(done.payload()["state"], "done");
    assert_eq!(done.payload()["decisions"][0], "shipped");

    // 200 on every one of them, per the status table.
    let (status, body) = post(srv.port(), "/v1/add", r#"{"title":"a second one"}"#);
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["display_id"],
        "QP-2",
        "a success body is the bare payload, with no wrapper"
    );
}

/// The client renders the prose, from a payload it never computed.
///
/// `human()` is not on the wire — the response is the same bytes whether the
/// caller asked for JSON or not — so a prose run proves the client rebuilt the
/// payload and rendered it locally.
#[test]
fn the_client_renders_the_prose_from_the_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let srv = Server::start(&store(tmp.path()));
    let out = std::process::Command::new(qp_bin())
        .env("QP_SERVER", &srv.url)
        .env("QP_DB", "/nonexistent/quipu/db.sqlite")
        .args(["add", "rendered here"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "QP-1\tready\trendered here"
    );
}

/// `add` returns a display id the caller could not have computed.
///
/// This is why `add` is in the slice. The id is minted inside the transaction
/// from the store's own rowid and prefix; a client with no store cannot guess
/// it, so the response carries information no status line could. It is also the
/// only way a wire test can build its own fixture — a client's local `add`
/// writes to a different file.
#[test]
fn add_returns_a_minted_id_the_client_could_not_have_computed() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path());
    // Six tickets already in the store, minted before this client existed.
    for i in 0..6 {
        local(&db, &["add", &format!("existing {i}")]);
    }
    let srv = Server::start(&db);
    let created = remote(&srv, &["add", "the seventh"]);
    assert_eq!(created.code, 0, "{}", created.stderr);
    assert_eq!(
        created.payload()["display_id"],
        "QP-7",
        "the minted id depends on store state the client cannot see"
    );
}

/// `complete` reports a promoted dependent in `_effects`, over the wire.
///
/// Locally the omission is invisible — the caller's next read hits the same
/// file. Remotely the caller is simply never told, and an orchestrator that
/// dispatches on "what just became ready" has to poll for an answer it could
/// have been handed.
#[test]
fn complete_reports_a_promoted_dependent_in_effects() {
    let tmp = tempfile::tempdir().unwrap();
    let srv = Server::start(&store(tmp.path()));

    remote(&srv, &["add", "blocker"]);
    remote(&srv, &["add", "dependent", "--depends-on", "QP-1"]);
    remote(&srv, &["assign", "QP-1", "--to", "w-a"]);
    remote(&srv, &["claim", "QP-1", "--as", "w-a"]);
    let done = remote(&srv, &["complete", "QP-1", "--as", "w-a"]);
    assert_eq!(done.code, 0, "{}", done.stderr);

    let effects = done.payload()["_effects"].clone();
    assert_eq!(
        effects,
        serde_json::json!([{
            "kind": "state_change", "task": "QP-2",
            "from": "pending", "to": "ready", "via": "complete"
        }]),
        "the promotion the caller never named must cross the socket"
    );

    // Absent, not empty, when nothing else moved — the `skip_serializing_if`
    // convention, which the client's deserialise has to tolerate.
    remote(&srv, &["add", "lonely"]);
    remote(&srv, &["assign", "QP-3", "--to", "w-a"]);
    remote(&srv, &["claim", "QP-3", "--as", "w-a"]);
    let lonely = remote(&srv, &["complete", "QP-3", "--as", "w-a"]);
    assert_eq!(lonely.code, 0, "{}", lonely.stderr);
    assert!(
        lonely.payload().get("_effects").is_none(),
        "`_effects` is present iff something else moved"
    );
}

// ---------------------------------------------------------------------------
// the error contract
// ---------------------------------------------------------------------------

/// Every reachable failure exits exactly as it does with no server in the path.
///
/// Two stores are driven through the identical setup — one locally, one over
/// the wire — and then the same failing command is run both ways. Exit code and
/// `kind` must agree. `invariant`/422 has no row here because no cycle is
/// reachable from these four verbs: a task being created cannot yet be pointed
/// at, so `add --depends-on` can never close one. It arrives with `depends`, in
/// the fan-out.
#[test]
fn every_failure_exits_the_same_as_it_does_locally() {
    let tmp = tempfile::tempdir().unwrap();
    let local_db = store(&tmp.path().join("a"));
    let served_db = store(&tmp.path().join("b"));
    let srv = Server::start(&served_db);

    let setup: &[&[&str]] = &[
        &["add", "held"],
        &["assign", "QP-1", "--to", "w-owner"],
        &["claim", "QP-1", "--as", "w-owner"],
        // The second task and the edge that makes a cycle reachable: QP-2 now
        // waits on QP-1, so pointing QP-1 back at QP-2 closes one.
        &["add", "downstream"],
        &["depends", "QP-2", "--on", "QP-1"],
    ];
    for step in setup {
        assert_eq!(local(&local_db, step).code, 0);
        let r = remote(&srv, step);
        assert_eq!(r.code, 0, "{}", r.stderr);
    }

    // (args, kind, exit)
    let cases: &[(&[&str], &str, i32)] = &[
        (&["assign", "QP-99", "--to", "w-x"], "not_found", 2),
        (&["assign", "QP-1", "--to", "w-x"], "conflict", 2),
        (&["claim", "QP-1", "--as", "w-other"], "conflict", 2),
        (&["complete", "QP-1", "--as", "w-other"], "not_owner", 2),
        (
            &["claim", "QP-1", "--as", "w-owner", "--for", "banana"],
            "invalid_input",
            1,
        ),
        // The fan-out verbs. Every one of these is a failure that leaves the
        // store untouched, so the two fixtures stay identical down the list.
        (&["abandon", "QP-1", "--as", "w-other"], "not_owner", 2),
        (&["reclaim", "QP-99"], "not_found", 2),
        (&["reclaim", "QP-1"], "conflict", 2),
        (&["cancel", "QP-99"], "not_found", 2),
        (
            &["block", "QP-99", "--as", "w-x", "--new", "a blocker"],
            "not_found",
            2,
        ),
        (&["log", "QP-99", "note", "into the void"], "not_found", 2),
        (&["tag", "QP-99", "--json", "add", "kind:x"], "not_found", 2),
        (
            &["tag", "QP-1", "--json", "add", "trailing:"],
            "invalid_input",
            1,
        ),
        (&["relation", "add", "QP-1", "", "QP-2"], "invalid_input", 1),
        (
            &["relation", "add", "QP-1", "variant-of", "QP-99"],
            "not_found",
            2,
        ),
        (&["relation", "list", "QP-99"], "not_found", 2),
        (&["depends", "QP-1", "--on", "QP-99"], "not_found", 2),
        // No `--as`, and QP-1 is running: the edge-ownership gate, not a lookup.
        (&["depends", "QP-1", "--on", "QP-2", "--rm"], "not_owner", 2),
        (&["contains", "QP-99", "QP-1"], "not_found", 2),
        (&["edit", "QP-99", "--title", "x"], "not_found", 2),
        (&["edit", "QP-1"], "invalid_input", 1),
        // The one kind the vertical slice could not reach: adding QP-1 → QP-2
        // on top of the setup's QP-2 → QP-1 closes a cycle, which is `invariant`
        // (422) rather than `conflict` — waiting will never make it succeed.
        (
            &["depends", "QP-1", "--on", "QP-2", "--as", "w-owner"],
            "invariant",
            2,
        ),
    ];
    for (args, kind, exit) in cases {
        let l = local(&local_db, args);
        let r = remote(&srv, args);
        assert_eq!(
            l.code,
            *exit,
            "local `qp {}` exited {} not {exit}: {}",
            args.join(" "),
            l.code,
            l.stderr
        );
        assert_eq!(
            r.code,
            l.code,
            "`qp {}` exits {} locally and {} through the server — the exit-code \
             contract is the one thing a server may never change",
            args.join(" "),
            l.code,
            r.code
        );
        assert_eq!(l.kind().as_deref(), Some(*kind), "local kind for {args:?}");
        assert_eq!(
            r.kind(),
            l.kind(),
            "kind for {args:?} differs over the wire"
        );
    }
}

/// The documented status for each kind, read off the status line itself.
#[test]
fn each_kind_carries_its_documented_http_status() {
    let tmp = tempfile::tempdir().unwrap();
    let srv = Server::start(&store(tmp.path()));
    let port = srv.port();
    post(port, "/v1/add", r#"{"title":"held"}"#);
    post(port, "/v1/assign", r#"{"task":"QP-1","to":"w-owner"}"#);

    // (path, body, status, kind)
    let cases: &[(&str, &str, u16, &str)] = &[
        ("/v1/add", r#"{"nope":1}"#, 400, "invalid_input"),
        (
            "/v1/assign",
            r#"{"task":"QP-99","to":"w"}"#,
            404,
            "not_found",
        ),
        ("/v1/assign", r#"{"task":"QP-1","to":"w"}"#, 409, "conflict"),
        (
            "/v1/claim",
            r#"{"task":"QP-1","agent":"w-x"}"#,
            409,
            "not_owner",
        ),
        ("/v1/nope", "{}", 404, "not_found"),
        (
            "/v1/add?project=absent",
            r#"{"title":"x"}"#,
            404,
            "no_store",
        ),
    ];
    for (path, body, status, kind) in cases {
        let (got, body) = post(port, path, body);
        let v: serde_json::Value = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("{path} body is not JSON ({e}): {body}"));
        assert_eq!(got, *status, "status for {path} {body}");
        assert_eq!(v["error"]["kind"], *kind, "kind for {path}");
    }

    // A verb path that exists still refuses a non-POST: it is the operator's
    // input, so it is `invalid_input`, not a 404 about a path that is there.
    let (status, _) = request(port, "GET /v1/add HTTP/1.1", None);
    assert_eq!(status, 400);

    // Every one of those was refused before or by the store, so each is on
    // stderr — that line is the only record of the ones the store never saw.
    let logged = srv
        .lines()
        .iter()
        .filter(|l| l.contains("\"event\":\"request\""))
        .count();
    assert!(
        logged >= cases.len(),
        "every 4xx must be logged; saw {logged}"
    );
}

/// `/healthz` answers from memory, touches no store, and takes no project.
#[test]
fn health_answers_from_process_local_state() {
    let tmp = tempfile::tempdir().unwrap();
    let srv = Server::start(&store(tmp.path()));
    let (status, body) = request(srv.port(), "GET /healthz HTTP/1.1", None);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert!(v["projects"].as_array().unwrap().len() == 1);
    assert!(v["bind"].as_str().unwrap().starts_with("127.0.0.1:"));
}

/// A verb the front door does not answer yet says so before reading local state.
///
/// It still runs — the wire contract keeps `init`, `export` and friends local
/// on purpose — but it runs against a *different store* from the one the
/// operator just named, and reading the wrong file silently is the failure the
/// attach check exists to prevent.
#[test]
fn an_unserved_verb_says_it_ran_locally() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path());
    let srv = Server::start(&store(&tmp.path().join("served")));

    let out = std::process::Command::new(qp_bin())
        .env("QP_SERVER", &srv.url)
        .env("QP_DB", &db)
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The project-mismatch warning may precede it; both are JSON Lines.
    let kinds: Vec<String> = stderr
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v["warning"]["kind"].as_str().map(str::to_string))
        .collect();
    assert!(
        kinds.iter().any(|k| k == "verb_not_served"),
        "expected a verb_not_served warning, got {kinds:?}"
    );

    // A served verb says nothing of the kind.
    let served = remote(&srv, &["add", "quiet"]);
    assert_eq!(served.code, 0);
    assert!(
        !served.stderr.contains("verb_not_served"),
        "a served verb must not warn: {}",
        served.stderr
    );
}

// ---------------------------------------------------------------------------
// the race
// ---------------------------------------------------------------------------

/// Several client processes race one ticket through the server.
///
/// Exactly one wins; the losers get 409/`conflict` and exit 2. The server adds
/// no serialisation of its own — `BEGIN IMMEDIATE` inside SQLite is the whole
/// guarantee, and it is the same guarantee whether the writers are processes,
/// threads, or a concurrent `qp` in someone's terminal.
///
/// The fixture is minted through the served `add`. Opening the server's file
/// out of band would work under SQLite and would test a setup path no user has.
#[test]
fn racing_clients_produce_exactly_one_winner() {
    const RACERS: usize = 5;

    let tmp = tempfile::tempdir().unwrap();
    let srv = Server::start(&store(tmp.path()));
    let created = remote(&srv, &["add", "the contested one"]);
    assert_eq!(created.code, 0, "{}", created.stderr);
    let id = created.payload()["display_id"]
        .as_str()
        .unwrap()
        .to_string();

    let children: Vec<Child> = (0..RACERS)
        .map(|_| {
            std::process::Command::new(qp_bin())
                .env("QP_SERVER", &srv.url)
                .env("QP_DB", "/nonexistent/quipu/db.sqlite")
                .args(["assign", &id, "--to", "w-racer", "--json"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();

    let mut winners = 0;
    let mut conflicts = 0;
    for ch in children {
        let out = ch.wait_with_output().unwrap();
        let code = out.status.code().unwrap_or(-1);
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        if code == 0 {
            winners += 1;
            continue;
        }
        assert_eq!(code, 2, "a loser must exit 2, not {code}: {err}");
        let v: serde_json::Value =
            serde_json::from_str(err.lines().find(|l| l.starts_with('{')).unwrap()).unwrap();
        assert_eq!(v["error"]["kind"], "conflict");
        assert_eq!(
            v["error"]["code"], "not_ready",
            "the stable error code must survive the wire; skills branch on it"
        );
        conflicts += 1;
    }
    assert_eq!(winners, 1, "exactly one racer may win");
    assert_eq!(conflicts, RACERS - 1);
}

// ---------------------------------------------------------------------------
// the remote-kill hazard
// ---------------------------------------------------------------------------

/// The serving process ignores `SIGPIPE`; a departing client cannot kill it.
///
/// Two halves, because one of them is not enough and it took a wrong turn to
/// find that out.
///
/// The *behavioural* half — connect, ask for a large response, never read it,
/// reset the connection — is the scenario, and the server must survive it. But
/// it does not by itself pin the fix: measured against a deliberately broken
/// build with `SIG_DFL` restored, it still passed, because the whole response
/// fits the server's autotuned socket send buffer, `write` returns success, and
/// no signal is ever raised. A response big enough to block the write is not
/// reachable through these verbs — the request that produces it would have to
/// exceed the body cap. So this half proves the process keeps serving, and
/// nothing more.
///
/// The *dispositional* half is what actually holds the fix in place, and it is
/// exact rather than probabilistic: `/proc/<pid>/status` publishes the ignored
/// signal mask, so the assertion is that bit 13 of `SigIgn` is set in the live
/// server. Under the CLI's own disposition it is clear, and that is precisely
/// the state in which some future client — a slower link, a larger payload, a
/// proxy in front — takes the whole process down with it.
#[test]
fn the_serving_process_survives_a_client_that_leaves_mid_response() {
    let tmp = tempfile::tempdir().unwrap();
    let srv = Server::start(&store(tmp.path()));
    let port = srv.port();

    let big = "x".repeat(900_000);
    for _ in 0..3 {
        let body = serde_json::json!({"title": "vanishing reader", "description": big}).to_string();
        let s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        shrink_receive_buffer(&s);
        let mut s = s;
        let req = format!(
            "POST /v1/add HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).expect("write request");
        s.flush().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        reset_and_close(s);
    }

    // Still there, and still answering everyone else.
    let after = remote(&srv, &["add", "still serving"]);
    assert_eq!(
        after.code, 0,
        "the server died with a client that walked away: {}",
        after.stderr
    );
    assert!(after.payload()["display_id"].is_string());
    let (status, _) = request(port, "GET /healthz HTTP/1.1", None);
    assert_eq!(status, 200, "healthz must still answer");

    assert!(
        sigpipe_is_ignored(srv.child.id()),
        "the serving process has SIGPIPE at its CLI disposition. Every client \
         that disconnects mid-response is then a remote shutdown of a process \
         serving everyone else — see `ignore_sigpipe` in src/cmd/serve.rs."
    );
}

/// Whether the live process with this pid ignores `SIGPIPE`.
///
/// Reads the kernel's own answer rather than inferring one from behaviour.
#[cfg(target_os = "linux")]
fn sigpipe_is_ignored(pid: u32) -> bool {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("read /proc status");
    let mask = status
        .lines()
        .find_map(|l| l.strip_prefix("SigIgn:"))
        .expect("SigIgn line");
    let bits = u64::from_str_radix(mask.trim(), 16).expect("hex mask");
    // The mask is 1-indexed by signal number, so SIGPIPE (13) is bit 12.
    bits & (1 << 12) != 0
}

#[cfg(not(target_os = "linux"))]
fn sigpipe_is_ignored(_pid: u32) -> bool {
    true
}

/// The CLI's own disposition is untouched, which is the other half of the fix.
///
/// `qp list | head -1` must keep dying like a Unix filter rather than panicking
/// at 101. `closed_stdout_pipe_never_exits_101` in `tests/cli.rs` pins the
/// behaviour; this pins the mechanism it rests on, so a future "just ignore it
/// everywhere" simplification fails here rather than there.
#[test]
#[cfg(target_os = "linux")]
fn the_cli_keeps_sigpipe_at_its_default_disposition() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path());
    // `wait` blocks on a poll loop, which is a live CLI process to inspect.
    let mut child = std::process::Command::new(qp_bin())
        .env("QP_DB", &db)
        .args(["wait", "--tag", "nothing", "--timeout", "10"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let ignored = sigpipe_is_ignored(child.id());
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        !ignored,
        "a CLI invocation must keep SIGPIPE at SIG_DFL; the serve path narrows \
         it, and `main` must not"
    );
}

/// Shrinks the receive window so the server blocks writing almost immediately.
#[cfg(unix)]
fn shrink_receive_buffer(s: &TcpStream) {
    use std::os::unix::io::AsRawFd;
    let size: libc::c_int = 2048;
    // SAFETY: `fd` is owned by `s` and outlives the call; `size` is a live
    // `c_int` of exactly the length passed.
    unsafe {
        libc::setsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            std::ptr::addr_of!(size).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}

/// Closes with `SO_LINGER 0`, so the peer sees a RESET rather than a clean FIN.
///
/// A half-close is not enough: a server may write to a socket that has sent FIN
/// without any error at all. The write must fail, and only a reset makes it.
#[cfg(unix)]
fn reset_and_close(s: TcpStream) {
    use std::os::unix::io::AsRawFd;
    let linger = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    // SAFETY: as above; `s` is dropped immediately afterwards, which is the
    // close that the option governs.
    unsafe {
        libc::setsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            std::ptr::addr_of!(linger).cast(),
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        );
    }
    drop(s);
}

// ---------------------------------------------------------------------------
// startup
// ---------------------------------------------------------------------------

/// A project whose store is missing refuses to start, and says which.
///
/// The failure this prevents: a process that binds successfully and *then*
/// finds it cannot open a store looks perfectly healthy to a supervisor —
/// running, accepting, probe green — while answering `no_store` to every
/// request for that project, forever.
#[test]
fn a_project_with_no_store_refuses_to_start() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("registry.json");

    // A name that is in no registry at all.
    let out = std::process::Command::new(qp_bin())
        .env("QUIPU_REGISTRY", &registry)
        .env("QP_DB", store(tmp.path()))
        .args([
            "serve",
            "--bind",
            "127.0.0.1:0",
            "--project",
            "not-registered",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(6),
        "a missing project is `no_store`, exit 6 — the same answer a CLI gets: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not-registered"),
        "the refusal must name the project that failed"
    );

    // A store path that is simply not there.
    let out = std::process::Command::new(qp_bin())
        .env("QP_DB", tmp.path().join("absent").join("db.sqlite"))
        .args(["serve", "--bind", "127.0.0.1:0"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(6));

    // And an unusable bind address is the operator's input: `invalid_input`, 1.
    let out = std::process::Command::new(qp_bin())
        .env("QP_DB", store(&tmp.path().join("c")))
        .args(["serve", "--bind", "not-an-address"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
}

// ---------------------------------------------------------------------------
// the fan-out: every remaining write verb, over the same socket
// ---------------------------------------------------------------------------

/// Every verb the fan-out added, end to end, asserting the payload it returns.
///
/// One store and one sequence rather than a test per verb: the interesting
/// half is that the payloads are the same ones the CLI produces, and a single
/// walk through the state machine exercises them against state the previous
/// step actually created.
#[test]
fn every_fanned_out_verb_round_trips_through_the_front_door() {
    let tmp = tempfile::tempdir().unwrap();
    let srv = Server::start(&store(tmp.path()));
    let ok = |r: &Run, what: &str| assert_eq!(r.code, 0, "{what} failed: {}", r.stderr);

    for title in ["root", "child", "spare"] {
        ok(&remote(&srv, &["add", title]), "add");
    }

    let edited = remote(
        &srv,
        &["edit", "QP-1", "--title", "renamed", "--tier", "slice"],
    );
    ok(&edited, "edit");
    assert_eq!(edited.payload()["changed"], true);
    assert_eq!(edited.payload()["changes"]["title"]["to"], "renamed");
    assert_eq!(edited.payload()["changes"]["tier"]["to"], "slice");

    let tagged = remote(&srv, &["tag", "QP-1", "--json", "add", "kind:wave"]);
    ok(&tagged, "tag add");
    assert_eq!(tagged.payload()["op"], "added");
    assert_eq!(tagged.payload()["name"], "kind:wave");
    let untagged = remote(&srv, &["tag", "QP-1", "--json", "rm", "kind:wave"]);
    ok(&untagged, "tag rm");
    assert_eq!(untagged.payload()["op"], "removed");

    let logged = remote(&srv, &["log", "QP-1", "note", "over the wire"]);
    ok(&logged, "log");
    assert_eq!(logged.payload()["kind"], "note");
    assert_eq!(logged.payload()["display_id"], "QP-1");

    let linked = remote(&srv, &["relation", "add", "QP-2", "variant-of", "QP-1"]);
    ok(&linked, "relation add");
    assert_eq!(linked.payload()["added"], true);
    let listed = remote(&srv, &["relation", "list", "QP-1"]);
    ok(&listed, "relation list");
    assert_eq!(
        listed.payload(),
        serde_json::json!({
            "outgoing": [],
            "incoming": [{"from": "QP-2", "kind": "variant-of"}]
        }),
        "the listing keeps the shape it has always had; the untagged outcome \
         enum must not wrap it"
    );
    let unlinked = remote(&srv, &["relation", "rm", "QP-2", "variant-of", "QP-1"]);
    ok(&unlinked, "relation rm");
    assert_eq!(unlinked.payload()["removed"], true);

    let held = remote(&srv, &["contains", "QP-1", "QP-2"]);
    ok(&held, "contains");
    assert_eq!(held.payload()["changed"], serde_json::json!(["QP-2"]));
    let released = remote(&srv, &["contains", "QP-1", "QP-2", "--rm"]);
    ok(&released, "contains --rm");
    assert_eq!(released.payload()["op"], "rm");
    assert_eq!(released.payload()["changed"], serde_json::json!(["QP-2"]));

    let dep = remote(&srv, &["depends", "QP-2", "--on", "QP-3"]);
    ok(&dep, "depends");
    assert_eq!(dep.payload()["op"], "add");
    assert_eq!(dep.payload()["on_id"], "QP-3");
    let undep = remote(&srv, &["depends", "QP-2", "--on", "QP-3", "--rm"]);
    ok(&undep, "depends --rm");
    assert_eq!(undep.payload()["op"], "rm");
    assert_eq!(
        undep.payload()["promoted"],
        true,
        "removing the last unresolved dep promotes the named task"
    );

    // `take` chooses; the caller never named a task, so the id in the response
    // is information the request did not contain.
    let took = remote(&srv, &["take", "--as", "w-taker"]);
    ok(&took, "take");
    assert_eq!(took.payload()["taken"]["display_id"], "QP-1");
    assert_eq!(took.payload()["taken"]["state"], "assigned");
    assert_eq!(took.payload()["agent_id"], "w-taker");

    let gone = remote(
        &srv,
        &["abandon", "QP-1", "--as", "w-taker", "--reason", "busy"],
    );
    ok(&gone, "abandon");
    assert_eq!(gone.payload()["display_id"], "QP-1");
    assert_eq!(gone.payload()["reason"], "busy");

    ok(&remote(&srv, &["assign", "QP-1", "--to", "w-b"]), "assign");
    ok(&remote(&srv, &["claim", "QP-1", "--as", "w-b"]), "claim");
    let back = remote(&srv, &["reclaim", "QP-1", "--force"]);
    ok(&back, "reclaim");
    assert_eq!(back.payload()["forced"], true);

    // `block` is the one verb here that needs the task in flight: it closes the
    // caller's own assignment, so a ready task is `not_blockable`.
    ok(&remote(&srv, &["assign", "QP-2", "--to", "w-b"]), "assign");
    ok(&remote(&srv, &["claim", "QP-2", "--as", "w-b"]), "claim");
    let blocked = remote(
        &srv,
        &["block", "QP-2", "--as", "w-b", "--new", "a real blocker"],
    );
    ok(&blocked, "block");
    assert_eq!(blocked.payload()["display_id"], "QP-2");
    assert_eq!(blocked.payload()["state"], "pending");
    assert_eq!(blocked.payload()["blocker_title"], "a real blocker");
    assert_eq!(
        blocked.payload()["blocker_tags"],
        serde_json::json!(["kind:blocker"])
    );

    let cancelled = remote(&srv, &["cancel", "QP-3", "--reason", "not needed"]);
    ok(&cancelled, "cancel");
    assert_eq!(cancelled.payload()["state"], "cancelled");
    assert_eq!(cancelled.payload()["reason"], "not needed");
}

/// 200 on the status line for every fanned-out verb, read off the wire.
///
/// Through raw POSTs rather than the client, for the reason `post` exists: the
/// status is what is under test. It doubles as the only place the *request*
/// body of a subcommand group is written out — `{"op":{"add":{...}}}` is what
/// a hand-written client has to send, and nothing else pins that shape.
#[test]
fn every_fanned_out_verb_answers_200() {
    let tmp = tempfile::tempdir().unwrap();
    let srv = Server::start(&store(tmp.path()));
    let port = srv.port();

    let steps: &[(&str, &str)] = &[
        ("/v1/add", r#"{"title":"one"}"#),
        ("/v1/add", r#"{"title":"two"}"#),
        ("/v1/edit", r#"{"task":"QP-1","title":"one renamed"}"#),
        (
            "/v1/tag",
            r#"{"task":"QP-1","op":{"add":{"name":"kind:x"}}}"#,
        ),
        (
            "/v1/tag",
            r#"{"task":"QP-1","op":{"rm":{"name":"kind:x"}}}"#,
        ),
        ("/v1/log", r#"{"task":"QP-1","kind":"note","body":"hi"}"#),
        (
            "/v1/relation",
            r#"{"op":{"add":{"from":"QP-2","kind":"variant-of","to":"QP-1"}}}"#,
        ),
        ("/v1/relation", r#"{"op":{"list":{"task":"QP-1"}}}"#),
        (
            "/v1/relation",
            r#"{"op":{"rm":{"from":"QP-2","kind":"variant-of","to":"QP-1"}}}"#,
        ),
        ("/v1/depends", r#"{"task":"QP-2","on":"QP-1"}"#),
        ("/v1/depends", r#"{"task":"QP-2","on":"QP-1","rm":true}"#),
        ("/v1/contains", r#"{"parent":"QP-1","children":["QP-2"]}"#),
        (
            "/v1/contains",
            r#"{"parent":"QP-1","children":["QP-2"],"rm":true}"#,
        ),
        ("/v1/take", r#"{"agent":"w-x"}"#),
        ("/v1/abandon", r#"{"task":"QP-1","agent":"w-x"}"#),
        ("/v1/assign", r#"{"task":"QP-1","to":"w-y"}"#),
        ("/v1/claim", r#"{"task":"QP-1","agent":"w-y"}"#),
        (
            "/v1/block",
            r#"{"task":"QP-1","agent":"w-y","new":"blocker"}"#,
        ),
        ("/v1/assign", r#"{"task":"QP-2","to":"w-z"}"#),
        ("/v1/reclaim", r#"{"task":"QP-2","force":true}"#),
        ("/v1/cancel", r#"{"task":"QP-2"}"#),
    ];
    for (path, body) in steps {
        let (status, got) = post(port, path, body);
        assert_eq!(status, 200, "{path} {body} answered {status}: {got}");
        assert!(
            serde_json::from_str::<serde_json::Value>(&got).is_ok(),
            "{path} body is not JSON: {got}"
        );
    }
}

/// `_effects` crosses the socket in both directions of travel.
///
/// Two verbs that reach it by opposite routes: `cancel` resolves a blocker and
/// the readiness sweep promotes its dependent, and `block` freezes a container's
/// subtree and demotes it. A caller with no store has no other way to be told
/// either happened.
#[test]
fn effects_cross_the_socket_for_promotions_and_demotions() {
    let tmp = tempfile::tempdir().unwrap();
    let srv = Server::start(&store(tmp.path()));

    // Promotion, via the readiness sweep: QP-2 waits on QP-1, and cancelling
    // QP-1 resolves it.
    remote(&srv, &["add", "blocker"]);
    remote(&srv, &["add", "dependent", "--depends-on", "QP-1"]);
    let cancelled = remote(&srv, &["cancel", "QP-1"]);
    assert_eq!(cancelled.code, 0, "{}", cancelled.stderr);
    assert_eq!(
        cancelled.payload()["_effects"],
        serde_json::json!([{
            "kind": "state_change", "task": "QP-2",
            "from": "pending", "to": "ready", "via": "cancel"
        }]),
        "the promotion the caller never named must cross the socket"
    );

    // Demotion, via `block`'s freeze: QP-3 contains QP-4, and blocking the
    // container freezes what is inside it.
    remote(&srv, &["add", "container"]);
    remote(&srv, &["add", "inside"]);
    // The container is taken in flight *before* it gains contents: a container
    // with an open child is pending, and a pending task cannot be blocked.
    remote(&srv, &["assign", "QP-3", "--to", "w-a"]);
    remote(&srv, &["claim", "QP-3", "--as", "w-a"]);
    remote(&srv, &["contains", "QP-3", "QP-4", "--as", "w-a"]);
    let blocked = remote(
        &srv,
        &["block", "QP-3", "--as", "w-a", "--new", "the reason"],
    );
    assert_eq!(blocked.code, 0, "{}", blocked.stderr);
    assert_eq!(
        blocked.payload()["_effects"],
        serde_json::json!([{
            "kind": "state_change", "task": "QP-4",
            "from": "ready", "to": "pending", "via": "block"
        }]),
        "`block` moves the subtree the other way, and that must cross too"
    );
}

/// An empty queue is a success with a null payload, not a 404.
///
/// The distinction a polling client depends on: "no work right now" and
/// "something broke" must never arrive as the same thing. Exit 0 locally, exit
/// 0 through the server, and 200 on the status line.
#[test]
fn take_on_an_empty_queue_is_success_over_the_wire() {
    let tmp = tempfile::tempdir().unwrap();
    let local_db = store(&tmp.path().join("a"));
    let srv = Server::start(&store(&tmp.path().join("b")));

    let l = local(&local_db, &["take", "--as", "w-poller"]);
    let r = remote(&srv, &["take", "--as", "w-poller"]);
    assert_eq!(l.code, 0, "local take on an empty queue: {}", l.stderr);
    assert_eq!(
        r.code, l.code,
        "an empty queue must exit the same through the server: {}",
        r.stderr
    );
    assert_eq!(r.payload(), l.payload());
    assert!(
        r.payload()["taken"].is_null(),
        "an empty queue is a null payload, not an error: {}",
        r.stdout
    );

    let (status, body) = post(srv.port(), "/v1/take", r#"{"agent":"w-poller"}"#);
    assert_eq!(
        status, 200,
        "nothing to take is 200; a 404 would make a poller retry a failure it \
         does not have: {body}"
    );

    // And the prose is the same sentence, rendered client-side from a payload
    // whose interesting field is `null`.
    let out = std::process::Command::new(qp_bin())
        .env("QP_SERVER", &srv.url)
        .env("QP_DB", "/nonexistent/quipu/db.sqlite")
        .args(["take", "--as", "w-poller"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "nothing to take"
    );
}

/// A dependency cycle is 422, the one status the first four verbs could not reach.
///
/// 422 rather than 409 because the two answer different questions: 409 means
/// "not now", and a cycle does not resolve by waiting.
#[test]
fn a_dependency_cycle_is_422_invariant() {
    let tmp = tempfile::tempdir().unwrap();
    let srv = Server::start(&store(tmp.path()));
    remote(&srv, &["add", "first"]);
    remote(&srv, &["add", "second"]);
    remote(&srv, &["depends", "QP-2", "--on", "QP-1"]);

    let (status, body) = post(srv.port(), "/v1/depends", r#"{"task":"QP-1","on":"QP-2"}"#);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(status, 422, "a cycle is 422: {body}");
    assert_eq!(v["error"]["kind"], "invariant");
}
