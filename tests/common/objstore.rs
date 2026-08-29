//! A hermetic object store: a `std::net::TcpListener` speaking enough S3 to serve a ledger.
//!
//! No network, no credentials, no dependency the crate did not already have.
//! Everything the ledger does — PUT, GET, ListObjectsV2 with prefixes,
//! delimiters and continuation tokens — goes through a real socket, a real
//! request line and a real XML response, so the client is exercised rather than
//! mocked out. The alternative on offer was a filesystem fake, which would have
//! left signing, url shape, pagination and error parsing entirely untested.
//!
//! It does not verify signatures. Verifying SigV4 here would mean writing a
//! second implementation and asserting the two agree, which proves nothing about
//! either; the signature is pinned against the published AWS vectors as a unit
//! test instead. What this does check is that a signature was PRESENT and had
//! the right shape, which is the failure a stub can actually see.
//!
//! The store is addressed as `s3://127.0.0.1:<port>/<bucket>` with
//! `QUIPU_S3_PLAINTEXT` set, because a `TcpListener` cannot terminate TLS
//! without dragging a TLS server and a certificate into the test tree.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The bucket every fixture uses. RFC 2606 style: nothing real is named here.
pub const BUCKET: &str = "quipu-ledger";

/// One request as the server saw it, for asserting on the wire format.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    /// Request target, path and query, exactly as it arrived.
    pub target: String,
    pub headers: Vec<(String, String)>,
}

impl Seen {
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }
    pub fn query(&self) -> &str {
        self.target.split_once('?').map(|(_, q)| q).unwrap_or("")
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Default)]
struct State {
    objects: BTreeMap<String, Vec<u8>>,
    seen: Vec<Seen>,
    /// Forced onto every response when set, so error handling can be exercised.
    fail: Option<(u16, String)>,
    /// Forced onto LISTs whose prefix starts with this, and nothing else.
    ///
    /// This is what a prefix-scoped key looks like from the client: the bucket
    /// root answers, one prefix inside it does not. A blanket `fail_with`
    /// cannot produce that, and the distinction between "no such bucket" and
    /// "your key is not scoped here" is undiagnosable without it.
    deny_prefix: Option<String>,
    /// Seconds added to the `Date` header, so a clock can be put out of true.
    date_skew: i64,
    /// Denied for this HTTP method only — a key that may read and not write.
    deny_method: Option<String>,
}

/// A running stub. Dropping it stops the listener thread.
pub struct ObjectStore {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    /// Keys returned per LIST page. Small values force the client to paginate.
    page: usize,
}

impl ObjectStore {
    pub fn start() -> Self {
        Self::with_page_size(1000)
    }

    pub fn with_page_size(page: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub object store");
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (s, st, pg) = (state.clone(), stop.clone(), page);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if st.load(Ordering::Relaxed) {
                    return;
                }
                match stream {
                    Ok(stream) => {
                        let _ = serve(stream, &s, pg);
                    }
                    Err(_) => return,
                }
            }
        });
        Self {
            addr,
            state,
            stop,
            page,
        }
    }

    /// The ledger url a store is configured with.
    pub fn url(&self) -> String {
        format!("s3://{}/{BUCKET}", self.addr)
    }

    /// The same store under a key prefix, to exercise the optional url path.
    pub fn url_with_prefix(&self, prefix: &str) -> String {
        format!("s3://{}/{BUCKET}/{prefix}", self.addr)
    }

    /// The port this stub is listening on.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The same stub addressed by a different host literal.
    ///
    /// For the one test that needs an address which REACHES loopback without
    /// BEING a loopback literal — `0.0.0.0`, which the kernel routes to
    /// localhost. That is the only way to observe the plaintext gate refusing a
    /// downgrade while still having a server on the other end to catch anything
    /// that leaked.
    pub fn url_at_host(&self, host: &str) -> String {
        format!("s3://{host}:{}/{BUCKET}", self.addr.port())
    }

    /// Every object key currently held, in order.
    pub fn keys(&self) -> Vec<String> {
        self.state.lock().unwrap().objects.keys().cloned().collect()
    }

    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.state.lock().unwrap().objects.get(key).cloned()
    }

    /// Puts an object no `qp` wrote, at a key of the fixture's choosing.
    ///
    /// The one way to fixture history from before the writer existed: a batch
    /// with no `project_key` cannot be produced by this binary any more, and a
    /// bucket that already holds one is exactly what every store looks like the
    /// first time it publishes after upgrading.
    pub fn place(&self, key: &str, body: &[u8]) {
        self.state
            .lock()
            .unwrap()
            .objects
            .insert(key.to_string(), body.to_vec());
    }

    /// A snapshot of the whole store, for asserting nothing was rewritten.
    pub fn snapshot(&self) -> BTreeMap<String, Vec<u8>> {
        self.state.lock().unwrap().objects.clone()
    }

    /// Every request served so far.
    pub fn requests(&self) -> Vec<Seen> {
        self.state.lock().unwrap().seen.clone()
    }

    pub fn forget_requests(&self) {
        self.state.lock().unwrap().seen.clear();
    }

    /// Makes every subsequent request fail with this S3 error code.
    pub fn fail_with(&self, status: u16, code: &str) {
        self.state.lock().unwrap().fail = Some((status, code.to_string()));
    }

    /// Denies only requests at or under `prefix`, the way a scoped key does.
    pub fn deny_prefix(&self, prefix: &str) {
        self.state.lock().unwrap().deny_prefix = Some(prefix.to_string());
    }

    /// Denies one HTTP method and serves the rest, the way a read-only key does.
    pub fn deny_method(&self, method: &str) {
        self.state.lock().unwrap().deny_method = Some(method.to_string());
    }

    /// Offsets the `Date` header every response carries, in seconds.
    ///
    /// The store keeps SERVING — a skew is only refused once it passes the
    /// signer's tolerance, and a doctor that can see the header should name the
    /// clock before the store ever refuses anything.
    pub fn skew_clock(&self, seconds: i64) {
        self.state.lock().unwrap().date_skew = seconds;
    }

    /// The environment a `qp` child needs to talk to this store.
    ///
    /// Credentials are obvious fakes. Nothing here can reach a real endpoint:
    /// the url is a loopback port that exists only while this object does.
    pub fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("AWS_ACCESS_KEY_ID", "AKIAIOSFODNN7EXAMPLE".to_string()),
            (
                "AWS_SECRET_ACCESS_KEY",
                "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            ),
            ("AWS_REGION", "us-east-1".to_string()),
            ("QUIPU_S3_PLAINTEXT", "1".to_string()),
        ]
    }

    pub fn page_size(&self) -> usize {
        self.page
    }
}

impl Drop for ObjectStore {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Unblock the accept loop so the thread notices the flag.
        let _ = std::net::TcpStream::connect(self.addr);
    }
}

fn serve(mut stream: TcpStream, state: &Arc<Mutex<State>>, page: usize) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();

    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 {
            break;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut body)?;
    }

    let (status, content, skew) = {
        let mut st = state.lock().unwrap();
        st.seen.push(Seen {
            method: method.clone(),
            target: target.clone(),
            headers: headers.clone(),
        });
        let skew = st.date_skew;
        let denied = st.deny_prefix.as_ref().is_some_and(|p| denies(&target, p))
            || st
                .deny_method
                .as_ref()
                .is_some_and(|m| m.eq_ignore_ascii_case(&method));
        if denied {
            (
                403,
                error_xml("AccessDenied", "denied by the test harness"),
                skew,
            )
        } else if let Some((code, name)) = st.fail.clone() {
            (code, error_xml(&name, "forced by the test harness"), skew)
        } else {
            let (c, b) = handle(&method, &target, body, &mut st, page);
            (c, b, skew)
        }
    };

    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Length: {}\r\nContent-Type: application/xml\r\n\
         Date: {}\r\nConnection: close\r\n\r\n",
        reason(status),
        content.len(),
        http_date(skew)
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&content)?;
    stream.flush()
}

fn handle(
    method: &str,
    target: &str,
    body: Vec<u8>,
    st: &mut State,
    page: usize,
) -> (u16, Vec<u8>) {
    let (raw_path, raw_query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    let path = decode(raw_path);
    let query = parse_query(raw_query);

    let rest = match path.strip_prefix('/') {
        Some(r) => r,
        None => return (400, error_xml("InvalidURI", "no path")),
    };
    let (bucket, key) = match rest.split_once('/') {
        Some((b, k)) => (b, k),
        None => (rest, ""),
    };
    if bucket != BUCKET {
        return (404, error_xml("NoSuchBucket", "no such bucket"));
    }

    if query.contains_key("list-type") {
        return list(st, &query, page);
    }
    match method {
        "PUT" => {
            st.objects.insert(key.to_string(), body);
            (200, Vec::new())
        }
        "GET" => match st.objects.get(key) {
            Some(v) => (200, v.clone()),
            None => (404, error_xml("NoSuchKey", "no such key")),
        },
        _ => (405, error_xml("MethodNotAllowed", "unsupported")),
    }
}

fn list(st: &State, query: &BTreeMap<String, String>, page: usize) -> (u16, Vec<u8>) {
    let prefix = query.get("prefix").cloned().unwrap_or_default();
    let delimiter = query.get("delimiter").cloned().unwrap_or_default();
    let after = query.get("continuation-token").cloned().unwrap_or_default();

    let mut contents: Vec<(&String, &Vec<u8>)> = Vec::new();
    let mut common: Vec<String> = Vec::new();
    for (k, v) in &st.objects {
        if !k.starts_with(&prefix) {
            continue;
        }
        if !delimiter.is_empty() {
            let tail = &k[prefix.len()..];
            if let Some(i) = tail.find(&delimiter) {
                let cp = format!("{prefix}{}{delimiter}", &tail[..i]);
                if !common.contains(&cp) {
                    common.push(cp);
                }
                continue;
            }
        }
        if !after.is_empty() && k.as_str() <= after.as_str() {
            continue;
        }
        contents.push((k, v));
    }

    let truncated = contents.len() > page;
    let shown: Vec<_> = contents.into_iter().take(page).collect();
    let next = truncated.then(|| shown.last().map(|(k, _)| (*k).clone()).unwrap_or_default());

    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
    );
    xml.push_str(&format!("<Name>{BUCKET}</Name>"));
    xml.push_str(&format!("<Prefix>{}</Prefix>", escape(&prefix)));
    xml.push_str("<MaxKeys>1000</MaxKeys>");
    xml.push_str(&format!(
        "<IsTruncated>{}</IsTruncated>",
        if truncated { "true" } else { "false" }
    ));
    xml.push_str(&format!("<KeyCount>{}</KeyCount>", shown.len()));
    for (k, v) in &shown {
        xml.push_str(&format!(
            "<Contents><Key>{}</Key><LastModified>2026-01-01T00:00:00.000Z</LastModified>\
             <ETag>&quot;{:x}&quot;</ETag><Size>{}</Size>\
             <StorageClass>STANDARD</StorageClass></Contents>",
            escape(k),
            v.len(),
            v.len()
        ));
    }
    for c in &common {
        xml.push_str(&format!(
            "<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>",
            escape(c)
        ));
    }
    if let Some(t) = next {
        xml.push_str(&format!(
            "<NextContinuationToken>{}</NextContinuationToken>",
            escape(&t)
        ));
    }
    xml.push_str("</ListBucketResult>");
    (200, xml.into_bytes())
}

/// Whether this request addresses `prefix` — a LIST of it, or an object under it.
///
/// Matched against the decoded target so both the `prefix=` query a LIST sends
/// and the key path a GET/PUT sends are covered by one knob.
fn denies(target: &str, prefix: &str) -> bool {
    let decoded = decode(target);
    let encoded_prefix = prefix.replace('/', "%2F");
    decoded.contains(prefix) || target.contains(&encoded_prefix)
}

/// The `Date` header, offset by `skew` seconds. RFC 9110 IMF-fixdate.
fn http_date(skew: i64) -> String {
    let t = time::OffsetDateTime::now_utc() + time::Duration::seconds(skew);
    let fmt = time::macros::format_description!(
        "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT"
    );
    t.format(fmt).expect("IMF-fixdate formats")
}

fn error_xml(code: &str, message: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code>\
         <Message>{message}</Message></Error>"
    )
    .into_bytes()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;")
}

fn parse_query(q: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for pair in q.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(decode(k), decode(v));
    }
    out
}

fn decode(s: &str) -> String {
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
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
