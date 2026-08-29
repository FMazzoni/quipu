//! The object-store transport the ledger lives on: PUT, GET and LIST over immutable keys.
//!
#![doc = include_str!("../docs/modules/s3.md")]

// `jiff` here is the signer's instant type, not a second datetime crate chosen
// over quipu's own: `rusty-s3` depends on it unconditionally and exposes it in
// `S3Action::sign_with_time`, so it is in the tree either way. `crate::time`
// remains the only source of RFC 3339 stamps that reach the store.
use anyhow::Result;
use rusty_s3::actions::{GetObject, ListObjectsV2, PutObject, S3Action};
use rusty_s3::{Bucket, Credentials, UrlStyle};
use std::path::Path;
use std::time::Duration;

/// Environment variables the credentials come from. Standard names, deliberately.
///
/// Quipu-specific names and a config file were both rejected: a machine that can
/// already reach an object store has these set, and a second spelling is a
/// second thing to get wrong. Quipu never writes a credential to disk.
pub const ENV_KEY: &str = "AWS_ACCESS_KEY_ID";
pub const ENV_SECRET: &str = "AWS_SECRET_ACCESS_KEY";
pub const ENV_TOKEN: &str = "AWS_SESSION_TOKEN";

/// Region names the signing scope, not a datacentre.
///
/// `s3://<host>/<bucket>` carries no region because the endpoint is named
/// outright; SigV4 still needs one in the credential scope, and every
/// S3-compatible server this targets accepts the default.
pub const ENV_REGION: &str = "AWS_REGION";
pub const ENV_REGION_ALT: &str = "AWS_DEFAULT_REGION";
const DEFAULT_REGION: &str = "us-east-1";

/// Permits plaintext http, and ONLY to a loopback endpoint. For the test harness.
///
/// `s3://` implies https and there is no plaintext url form — that is settled.
/// This exists because the hermetic end-to-end test is a `std::net::TcpListener`
/// stub, and a stub cannot terminate TLS without dragging a TLS server and a
/// certificate into the test tree. Nothing in quipu sets it.
///
/// **Setting it is not enough, and that is the point.** The host must also be a
/// loopback literal. A variable alone is a switch that travels: exported once in
/// a shell profile, or inherited by an agent, it would silently downgrade every
/// request to every endpoint thereafter — and because quipu presigns, the
/// signature rides in the query string, so one captured url is replayable for
/// the whole of [`SIGNATURE_TTL`] with the key's full rights. Plaintext to a
/// real endpoint is credential disclosure, not a weaker cipher.
///
/// Loopback is the boundary rather than operator discipline because it is the
/// one condition under which "an eavesdropper on the path" is not a coherent
/// threat: the packets never leave the machine. It is decided from the host as
/// WRITTEN, never resolved — a name that resolves to loopback today is a lever
/// someone else can move tomorrow, and DNS is not an authorisation mechanism.
pub const ENV_PLAINTEXT: &str = "QUIPU_S3_PLAINTEXT";

/// How long a presigned url stays valid. Sent immediately; this is slack, not a lease.
const SIGNATURE_TTL: Duration = Duration::from_secs(300);

/// Ceiling on one LIST page. The server may return fewer; pagination handles it.
const PAGE_SIZE: usize = 1000;

/// Every way a ledger request can fail, kept apart rather than flattened.
///
/// The variants are the point. `qp remote doctor` can only distinguish causes
/// this type preserved, and "request failed" is the one answer that helps
/// nobody: credentials absent and credentials rejected want opposite fixes, a
/// missing bucket and a key outside your policy look identical from a 403, and
/// SigV4's response to a clock 20 minutes out is an opaque signature error that
/// sends people to rotate keys that were never wrong.
#[derive(Debug, thiserror::Error)]
pub enum Fault {
    #[error("{0}")]
    BadUrl(String),

    #[error(
        "no object-store credentials in the environment; looked for {ENV_KEY} and \
         {ENV_SECRET} (plus {ENV_TOKEN} when the credentials are temporary). \
         quipu never stores a credential on disk and has no config file for one."
    )]
    NoCredentials,

    #[error(
        "the object store rejected the credentials ({code}). {ENV_KEY} and \
         {ENV_SECRET} are set, so this is the wrong key or the wrong secret \
         rather than a missing one."
    )]
    CredentialsRejected { code: String },

    #[error(
        "the object store rejected the request's timestamp ({code}): this machine's \
         clock is too far from the server's. SigV4 refuses a signature more than \
         about 15 minutes out, and the raw error does not say so."
    )]
    ClockSkew { code: String },

    #[error("the object store has no bucket named `{bucket}`")]
    BucketMissing { bucket: String },

    #[error(
        "the object store refused access to `{key}`. The bucket exists, so the \
         credentials are not scoped to this key prefix."
    )]
    Denied { key: String },

    #[error("no object at `{key}`")]
    NotFound { key: String },

    #[error(
        "TLS failure talking to {endpoint}: {detail}. The endpoint answered, so this is \
         a certificate or trust problem rather than an unreachable host."
    )]
    Tls { endpoint: String, detail: String },

    #[error("could not reach the object store at {endpoint}: {detail}")]
    Unreachable { endpoint: String, detail: String },

    #[error("the object store answered {status} for `{key}`{}", suffix(.code, .message))]
    Server {
        status: u16,
        key: String,
        code: Option<String>,
        message: Option<String>,
    },

    #[error("the object store's answer for `{key}` could not be read: {detail}")]
    Malformed { key: String, detail: String },
}

fn suffix(code: &Option<String>, message: &Option<String>) -> String {
    match (code, message) {
        (Some(c), Some(m)) => format!(" ({c}: {m})"),
        (Some(c), None) => format!(" ({c})"),
        _ => String::new(),
    }
}

/// A ledger url taken apart: `s3://<host>/<bucket>[/<prefix>]`.
///
/// The scheme picks the transport, the host is the endpoint, and the path is the
/// bucket plus an optional key prefix. There is no plaintext scheme and no
/// region in the url — see [`ENV_PLAINTEXT`] and [`ENV_REGION`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub host: String,
    pub bucket: String,
    /// Key prefix inside the bucket, without leading or trailing `/`. May be empty.
    pub prefix: String,
}

/// The one scheme a ledger url may have.
pub const SCHEME: &str = "s3://";

/// Splits a ledger url, or says exactly which part is wrong.
pub fn parse(url: &str) -> Result<Location, Fault> {
    let rest = url.strip_prefix(SCHEME).ok_or_else(|| {
        Fault::BadUrl(format!(
            "`{url}` is not a ledger url. The ledger lives in object storage and the \
             url form is `s3://<host>/<bucket>[/<prefix>]` — https is implied."
        ))
    })?;
    if rest.chars().any(char::is_control) || rest.trim() != rest {
        return Err(Fault::BadUrl(format!(
            "`{url}` has whitespace or a control character in it"
        )));
    }
    let mut parts = rest.splitn(3, '/');
    let host = parts.next().unwrap_or_default();
    let bucket = parts.next().unwrap_or_default();
    let prefix = parts.next().unwrap_or_default();
    if host.is_empty() {
        return Err(Fault::BadUrl(format!(
            "`{url}` names no host: the form is `s3://<host>/<bucket>[/<prefix>]`"
        )));
    }
    if bucket.is_empty() {
        return Err(Fault::BadUrl(format!(
            "`{url}` names a host but no bucket: the form is `s3://<host>/<bucket>[/<prefix>]`"
        )));
    }
    Ok(Location {
        host: host.to_string(),
        bucket: bucket.to_string(),
        prefix: prefix.trim_matches('/').to_string(),
    })
}

/// Whether a url is a ledger url at all, without deciding whether it is a good one.
pub fn is_object_url(url: &str) -> bool {
    url.starts_with(SCHEME)
}

/// The two verbs quipu issues against an object. There is deliberately no third.
///
/// The ledger is append-only and immutable, so DELETE is not implemented: a
/// batch is named after the store that wrote it and the event range it closes
/// over, and nothing quipu does can make one wrong afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Get,
    Put,
}

/// Signs one object request. The whole of quipu's SigV4 surface, in one place.
///
/// Split out as a free function so the published AWS test vectors can be pinned
/// against the exact call the client makes, rather than against the library
/// underneath it — a wrong region, a wrong url style or the wrong credential
/// reaching the signer are all invisible to a test that signs its own request.
fn sign(
    bucket: &Bucket,
    creds: &Credentials,
    op: Op,
    key: &str,
    expires_in: Duration,
    at: jiff::Timestamp,
) -> url::Url {
    match op {
        Op::Get => GetObject::new(bucket, Some(creds), key).sign_with_time(expires_in, &at),
        Op::Put => PutObject::new(bucket, Some(creds), key).sign_with_time(expires_in, &at),
    }
}

/// A bucket, a prefix, credentials, and a blocking HTTP agent. No connection is held.
///
/// Constructing one performs no I/O — it is the parse and the credential lookup,
/// nothing else — so a store that never syncs never pays for having a ledger
/// configured.
pub struct Client {
    bucket: Bucket,
    creds: Credentials,
    /// Key prefix from the url, normalised to end in `/` or be empty.
    root: String,
    endpoint: String,
    agent: ureq::Agent,
}

impl Client {
    /// Opens a client for a ledger url, reading credentials from the environment.
    pub fn open(url: &str) -> Result<Self, Fault> {
        let loc = parse(url)?;
        let creds = credentials()?;
        let scheme = scheme_for(&loc.host, plaintext_requested());
        let endpoint = format!("{scheme}://{}", loc.host);
        let parsed = endpoint
            .parse::<url::Url>()
            .map_err(|e| Fault::BadUrl(format!("`{url}` names an unusable host: {e}")))?;
        // Path style, not virtual-host style: the endpoint is a named host that
        // is usually not AWS, and `<bucket>.<host>` is a DNS name nobody
        // created. Every S3-compatible server accepts path style.
        let bucket = Bucket::new(parsed, UrlStyle::Path, loc.bucket.clone(), region())
            .map_err(|e| Fault::BadUrl(format!("`{url}` is not a usable bucket url: {e:?}")))?;
        let root = if loc.prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", loc.prefix)
        };
        Ok(Self {
            bucket,
            creds,
            root,
            endpoint,
            agent: ureq::Agent::config_builder()
                // The S3 error body is the only place the cause lives; turning
                // a 403 into an opaque `StatusCode` error throws away exactly
                // what tells credentials-rejected apart from not-scoped.
                .http_status_as_error(false)
                .build()
                .into(),
        })
    }

    /// Absolute key for a path relative to the url's prefix.
    fn abs(&self, key: &str) -> String {
        format!("{}{key}", self.root)
    }

    /// Uploads one object. Callers must only ever write a key that does not exist.
    pub fn put(&self, key: &str, body: &[u8]) -> Result<(), Fault> {
        let abs = self.abs(key);
        let url = sign(
            &self.bucket,
            &self.creds,
            Op::Put,
            &abs,
            SIGNATURE_TTL,
            now(),
        );
        let (status, resp) = self.send_put(url.as_str(), body, &abs)?;
        self.check(status, &resp, &abs)
    }

    /// Downloads one object whole.
    pub fn get(&self, key: &str) -> Result<Vec<u8>, Fault> {
        let abs = self.abs(key);
        let url = sign(
            &self.bucket,
            &self.creds,
            Op::Get,
            &abs,
            SIGNATURE_TTL,
            now(),
        );
        let (status, body) = self.send_get(url.as_str(), &abs)?;
        self.check(status, &body, &abs)?;
        Ok(body)
    }

    /// Every key under `sub`, relative to the url's prefix, following continuation tokens.
    ///
    /// This is the whole reason the ledger moved off git. A fetch takes a branch
    /// whole; a LIST names one prefix, so a store reads its own project and
    /// nothing else, and a project nobody has touched in a year costs nothing.
    pub fn list(&self, sub: &str) -> Result<Vec<String>, Fault> {
        let full = self.abs(sub);
        let mut token: Option<String> = None;
        let mut out = Vec::new();
        loop {
            let mut action = ListObjectsV2::new(&self.bucket, Some(&self.creds));
            action.with_prefix(full.clone());
            action.with_max_keys(PAGE_SIZE);
            if let Some(t) = &token {
                action.with_continuation_token(t.clone());
            }
            let url = action.sign_with_time(SIGNATURE_TTL, &now());
            let (status, body) = self.send_get(url.as_str(), &full)?;
            self.check(status, &body, &full)?;
            let text = String::from_utf8_lossy(&body).to_string();
            let parsed = ListObjectsV2::parse_response(&text).map_err(|e| Fault::Malformed {
                key: full.clone(),
                detail: format!("listing was not a ListObjectsV2 response: {e}"),
            })?;
            for c in parsed.contents {
                if let Some(rel) = c.key.strip_prefix(&self.root) {
                    out.push(rel.to_string());
                }
            }
            match parsed.next_continuation_token {
                Some(t) => token = Some(t),
                None => break,
            }
        }
        out.sort();
        Ok(out)
    }

    /// One HTTP round trip, with the transport's own failures kept apart.
    ///
    /// GET and PUT are separate calls rather than a method parameter because
    /// `ureq` types a body-carrying request differently from one without, and
    /// erasing that distinction here would buy nothing: the ledger has exactly
    /// these two verbs and will never have a third.
    fn send_get(&self, url: &str, key: &str) -> Result<(u16, Vec<u8>), Fault> {
        let resp = self.agent.get(url).call();
        self.read(resp, key)
    }

    fn send_put(&self, url: &str, body: &[u8], key: &str) -> Result<(u16, Vec<u8>), Fault> {
        let resp = self.agent.put(url).send(body);
        self.read(resp, key)
    }

    fn read(
        &self,
        resp: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
        key: &str,
    ) -> Result<(u16, Vec<u8>), Fault> {
        let (status, bytes, _) = self.read_dated(resp, key)?;
        Ok((status, bytes))
    }

    /// `read`, keeping the server's `Date` header instead of discarding it.
    ///
    /// The header is read off the response whatever its status, because the
    /// diagnosis that needs it is exactly the one where the request FAILED: a
    /// clock 20 minutes out is refused with a signature error, and the `Date`
    /// on that refusal is the only evidence the clock — not the key — is wrong.
    fn read_dated(
        &self,
        resp: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
        key: &str,
    ) -> Result<(u16, Vec<u8>, Option<String>), Fault> {
        let mut resp = resp.map_err(|e| self.transport_fault(e))?;
        let status = resp.status().as_u16();
        let date = resp
            .headers()
            .get("date")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = resp
            .body_mut()
            .with_config()
            .limit(u64::MAX)
            .read_to_vec()
            .map_err(|e| Fault::Malformed {
                key: key.to_string(),
                detail: e.to_string(),
            })?;
        Ok((status, bytes, date))
    }

    /// Sorts a `ureq` failure into the cause a person can act on.
    ///
    /// TLS and unreachable are separated deliberately: they look the same from a
    /// failed request and want completely different fixes — a trust store versus
    /// a firewall or a typo in the host.
    fn transport_fault(&self, e: ureq::Error) -> Fault {
        let endpoint = self.endpoint.clone();
        match e {
            ureq::Error::Tls(d) => Fault::Tls {
                endpoint,
                detail: d.to_string(),
            },
            ureq::Error::Rustls(d) => Fault::Tls {
                endpoint,
                detail: d.to_string(),
            },
            ureq::Error::HostNotFound => Fault::Unreachable {
                endpoint,
                detail: "the host does not resolve".to_string(),
            },
            other => Fault::Unreachable {
                endpoint,
                detail: other.to_string(),
            },
        }
    }

    /// Turns an S3 status plus its error body into a named cause.
    fn check(&self, status: u16, body: &[u8], key: &str) -> Result<(), Fault> {
        if (200..300).contains(&status) {
            return Ok(());
        }
        let text = String::from_utf8_lossy(body);
        let code = xml_field(&text, "Code");
        let message = xml_field(&text, "Message");
        let named = code.as_deref().unwrap_or("");
        // Skew first: the server reports it as a plain 403 whose code is often
        // indistinguishable from a bad secret, and the message is the only tell.
        if named == "RequestTimeTooSkewed"
            || named == "AccessDenied" && mentions_time(message.as_deref())
        {
            return Err(Fault::ClockSkew {
                code: named.to_string(),
            });
        }
        match named {
            "InvalidAccessKeyId"
            | "SignatureDoesNotMatch"
            | "InvalidSecurity"
            | "ExpiredToken"
            | "TokenRefreshRequired" => {
                return Err(Fault::CredentialsRejected {
                    code: named.to_string(),
                })
            }
            "NoSuchBucket" => {
                return Err(Fault::BucketMissing {
                    bucket: self.bucket.name().to_string(),
                })
            }
            "AccessDenied" => {
                return Err(Fault::Denied {
                    key: key.to_string(),
                })
            }
            "NoSuchKey" => {
                return Err(Fault::NotFound {
                    key: key.to_string(),
                })
            }
            _ => {}
        }
        if status == 404 {
            return Err(Fault::NotFound {
                key: key.to_string(),
            });
        }
        if status == 403 {
            return Err(Fault::Denied {
                key: key.to_string(),
            });
        }
        Err(Fault::Server {
            status,
            key: key.to_string(),
            code,
            message,
        })
    }
}

/// Whether an S3 error message is really complaining about the clock.
fn mentions_time(message: Option<&str>) -> bool {
    let Some(m) = message else { return false };
    let m = m.to_ascii_lowercase();
    m.contains("skew") || m.contains("expired") || m.contains("future")
}

/// One element's text out of a short S3 error document.
///
/// Substring extraction rather than an XML parser: this runs only on an error
/// body, the shape is two flat elements, and a parse failure here would replace
/// a real diagnosis with a parser complaint.
fn xml_field(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    let v = text[start..end].trim();
    (!v.is_empty()).then(|| v.to_string())
}

fn now() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

/// The url scheme to use for `host`, given whether the harness variable is set.
///
/// Pure, and takes the flag rather than reading it, so the decision is testable
/// without mutating a process-global environment that parallel tests share.
fn scheme_for(host: &str, plaintext_requested: bool) -> &'static str {
    if plaintext_requested && is_loopback_literal(host) {
        "http"
    } else {
        "https"
    }
}

fn plaintext_requested() -> bool {
    std::env::var_os(ENV_PLAINTEXT).is_some_and(|v| v != "0" && !v.is_empty())
}

/// Whether a url authority names loopback outright — no DNS, no resolution.
///
/// `127.0.0.0/8`, `::1`, and the literal `localhost`. Anything else is a real
/// endpoint as far as this decision is concerned, including a name that happens
/// to resolve to loopback right now.
fn is_loopback_literal(authority: &str) -> bool {
    let host = host_without_port(authority);
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// Strips the optional `:port` from a url authority, bracketed IPv6 included.
///
/// A bare IPv6 literal carries colons of its own, so "split on the colon" is
/// wrong in exactly the case that matters: `::1` would become the empty host,
/// parse as nothing, and be treated as a real endpoint.
fn host_without_port(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match authority.rsplit_once(':') {
        // More than one colon and no brackets: a bare IPv6 literal, port and all
        // ambiguous. Take it whole rather than guessing.
        Some(_) if authority.matches(':').count() > 1 => authority,
        Some((host, _)) => host,
        None => authority,
    }
}

fn region() -> String {
    std::env::var(ENV_REGION)
        .or_else(|_| std::env::var(ENV_REGION_ALT))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_REGION.to_string())
}

/// Credentials from the environment, or a named refusal.
///
/// Names the variables it looked for rather than saying "not configured": the
/// person seeing this has usually exported one of the two, or exported them into
/// a different shell.
fn credentials() -> Result<Credentials, Fault> {
    Credentials::from_env().ok_or(Fault::NoCredentials)
}

// ---------------------------------------------------------------------------
// key layout
// ---------------------------------------------------------------------------

/// The key one batch lives at: `<project key>/<store uuid>/<batch file>`.
///
/// The project key comes FIRST, and that ordering is the entire scaling
/// argument: a GET or a LIST names one prefix, so a store reads its own project
/// and never pays for anyone else's. Put the store uuid first instead and every
/// machine would have to enumerate the whole bucket to find its own project
/// again — which is the O(N^2) shape the git ledger had.
pub fn object_key(project_key: &str, store_uuid: &str, batch_file: &str) -> String {
    format!(
        "{}/{}/{batch_file}",
        encode_segment(project_key),
        encode_segment(store_uuid)
    )
}

/// The prefix holding every batch of one project.
pub fn project_prefix(project_key: &str) -> String {
    format!("{}/", encode_segment(project_key))
}

/// The prefix holding every batch one store wrote.
pub fn store_prefix(project_key: &str, store_uuid: &str) -> String {
    format!(
        "{}/{}/",
        encode_segment(project_key),
        encode_segment(store_uuid)
    )
}

/// Percent-encodes anything that would end a key segment early or need quoting.
///
/// A project key is user-supplied (`qp init --project-key`) and only has to
/// survive a batch header, so it may legitimately contain a `/` — which would
/// silently split one project across two prefixes and break the scoping this
/// whole layout exists to provide.
fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The `<store uuid>/<file>` tail of a key, for laying a download out on disk.
///
/// `None` for anything that is not a batch key, which a shared bucket may
/// legitimately hold — someone else's README, a lifecycle marker, a directory
/// placeholder object.
pub fn key_tail(key: &str) -> Option<(String, String)> {
    let mut parts = key.split('/');
    let _project = parts.next()?;
    let store = parts.next()?;
    let file = parts.next()?;
    if parts.next().is_some() || store.is_empty() || !file.ends_with(".jsonl") {
        return None;
    }
    Some((store.to_string(), file.to_string()))
}

/// Writes a downloaded batch under `dir` as `<store uuid>/<file>`.
///
/// The layout is not cosmetic: `import` checks a batch's filename against the
/// range its header declares, and `project::batches_by_project` walks exactly
/// one directory level. Flattening the download would make both read a ledger
/// that does not exist.
pub fn place(dir: &Path, key: &str, bytes: &[u8]) -> Result<()> {
    let Some((store, file)) = key_tail(key) else {
        return Ok(());
    };
    let sub = dir.join(store);
    std::fs::create_dir_all(&sub)?;
    std::fs::write(sub.join(file), bytes)?;
    Ok(())
}

/// Every first-level key segment the ledger holds, from one delimited LIST.
///
/// One request, no downloads: the project key is the first key segment, so the
/// server's `CommonPrefixes` answer is the candidate list, and enumerating stays
/// affordable on a bucket holding hundreds of projects.
///
/// Candidates, not projects. A segment is a LOCATION — a url naming the bucket
/// root cannot tell a project apart from a key prefix another store publishes
/// under. `project::scan_object` is what turns this into an answer, by confirming
/// each candidate against a batch header; callers wanting projects want that.
impl Client {
    pub fn list_project_keys(&self) -> Result<Vec<String>, Fault> {
        let mut action = ListObjectsV2::new(&self.bucket, Some(&self.creds));
        if !self.root.is_empty() {
            action.with_prefix(self.root.clone());
        }
        action.with_delimiter("/");
        action.with_max_keys(PAGE_SIZE);
        let url = action.sign_with_time(SIGNATURE_TTL, &now());
        let (status, body) = self.send_get(url.as_str(), &self.root)?;
        self.check(status, &body, &self.root)?;
        let text = String::from_utf8_lossy(&body).to_string();
        let parsed = ListObjectsV2::parse_response(&text).map_err(|e| Fault::Malformed {
            key: self.root.clone(),
            detail: format!("listing was not a ListObjectsV2 response: {e}"),
        })?;
        let mut out = Vec::new();
        for p in parsed.common_prefixes {
            let Some(rel) = p.prefix.strip_prefix(&self.root) else {
                continue;
            };
            let seg = rel.trim_end_matches('/');
            if !seg.is_empty() {
                out.push(decode_segment(seg));
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }
}

/// Which part of the bucket a diagnostic LIST addresses.
///
/// The two are not interchangeable, and the difference is a diagnosis: a key
/// scoped to one prefix is denied at the root while listing its own prefix
/// perfectly well, which is precisely how "the bucket is missing" and "the key
/// is not scoped to this prefix" are told apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The prefix the ledger url names — what every real ledger request uses.
    Prefix,
    /// The bucket root, ignoring the url's prefix. Diagnosis only.
    Root,
}

/// One diagnostic LIST: the outcome, plus the server's clock if it sent one.
///
/// The two travel together because they come from one response and one of them
/// explains the other.
pub struct Answer {
    /// The raw `Date` header. `None` when the server sent none or an unusable
    /// one — which leaves the clock UNCHECKED, never "skewed".
    pub server_date: Option<String>,
    pub result: Result<(), Fault>,
}

impl Client {
    /// A one-key LIST, for diagnosis rather than for reading the ledger.
    ///
    /// `max-keys=1` because nothing here wants the contents: the question is
    /// whether the request is signed, trusted, authorised and answered, and the
    /// first key settles all four. A transport failure is returned in `result`
    /// like any other, so a caller can tell "never got an answer" from "was
    /// answered no" without a second code path.
    pub fn probe(&self, scope: Scope) -> Answer {
        let prefix = match scope {
            Scope::Prefix => self.root.clone(),
            Scope::Root => String::new(),
        };
        let mut action = ListObjectsV2::new(&self.bucket, Some(&self.creds));
        if !prefix.is_empty() {
            action.with_prefix(prefix.clone());
        }
        action.with_max_keys(1);
        let url = action.sign_with_time(SIGNATURE_TTL, &now());
        match self.read_dated(self.agent.get(url.as_str()).call(), &prefix) {
            Err(e) => Answer {
                server_date: None,
                result: Err(e),
            },
            Ok((status, body, date)) => Answer {
                server_date: date,
                result: self.check(status, &body, &prefix),
            },
        }
    }

    /// The bucket this client is pointed at, for a diagnosis that must name it.
    pub fn bucket_name(&self) -> &str {
        self.bucket.name()
    }

    /// The key prefix from the url, normalised — empty when the url named none.
    pub fn prefix(&self) -> &str {
        &self.root
    }

    /// The endpoint url, scheme included, as requests are actually addressed.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

/// Reverses [`encode_segment`], so a project key survives the round trip to a key.
fn decode_segment(s: &str) -> String {
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

/// A temp directory that removes itself, downloaded batches and all.
///
/// The ledger has no local mirror by design, so everything a read materialises
/// is throwaway. It lives under the system temp directory rather than beside the
/// store so a crash cannot leave a half-downloaded ledger inside `.quipu/`,
/// where the next reader would find something that looks like state.
pub struct Scratch(std::path::PathBuf);

impl Scratch {
    pub fn new(what: &str) -> Result<Self> {
        let nonce = crate::time::now_rfc3339().replace([':', '.', '-'], "");
        let dir = std::env::temp_dir().join(format!("quipu-{what}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published AWS presigned-url examples, signed through quipu's own path.
    ///
    /// Pinned here rather than trusted from the signing crate's own suite
    /// because these go through [`sign`], which is the function the client
    /// calls: a wrong region, a wrong url style, a swapped key and secret, a
    /// GET signed as a PUT, or a changed signature lifetime all fail this and
    /// none of them are visible to a test that signs its own request.
    ///
    /// Source: AWS "Authenticating Requests: Using Query Parameters (AWS
    /// Signature Version 4)", the `examplebucket/test.txt` examples dated
    /// 2013-05-24T00:00:00Z with a 24-hour expiry.
    #[test]
    fn signing_matches_the_published_aws_vectors() {
        let at = jiff::Timestamp::from_second(1_369_353_600).unwrap();
        let day = Duration::from_secs(86400);
        let bucket = Bucket::new(
            "https://s3.amazonaws.com".parse().unwrap(),
            UrlStyle::VirtualHost,
            "examplebucket",
            "us-east-1",
        )
        .unwrap();
        let creds = Credentials::new(
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        );

        let common = "https://examplebucket.s3.amazonaws.com/test.txt\
             ?X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request\
             &X-Amz-Date=20130524T000000Z&X-Amz-Expires=86400&X-Amz-SignedHeaders=host\
             &X-Amz-Signature=";

        let get = sign(&bucket, &creds, Op::Get, "test.txt", day, at);
        assert_eq!(
            get.as_str(),
            format!("{common}aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"),
            "quipu signs a GET differently from the published AWS example"
        );

        let put = sign(&bucket, &creds, Op::Put, "test.txt", day, at);
        assert_eq!(
            put.as_str(),
            format!("{common}f4db56459304dafaa603a99a23c6bea8821890259a65c18ff503a4a72a80efd9"),
            "quipu signs a PUT differently from the published AWS example"
        );
        assert_ne!(
            get.as_str(),
            put.as_str(),
            "GET and PUT must not sign identically; the method is part of the \
             canonical request"
        );
    }

    /// The harness variable downgrades loopback and nothing else.
    ///
    /// The second half is the security-relevant one: quipu presigns, so the
    /// signature travels in the query string, and one plaintext request to a
    /// real endpoint hands an eavesdropper a replayable url. A variable set once
    /// in a profile must not be able to cause that.
    #[test]
    fn plaintext_is_permitted_for_loopback_and_refused_for_everything_else() {
        for host in [
            "127.0.0.1",
            "127.0.0.1:9000",
            "127.0.0.53",
            "localhost",
            "LocalHost:9000",
            "[::1]",
            "[::1]:9000",
            "::1",
        ] {
            assert_eq!(
                scheme_for(host, true),
                "http",
                "{host} is loopback and the harness asked for plaintext"
            );
            assert_eq!(
                scheme_for(host, false),
                "https",
                "{host} was downgraded without the variable being set"
            );
        }

        for host in [
            "minio.host.example",
            "minio.host.example:9000",
            "192.168.1.10",
            "10.0.0.1:9000",
            "0.0.0.0:9000",
            "[2001:db8::1]:9000",
            "localhost.host.example",
            "127.0.0.1.host.example",
            "notlocalhost",
            "",
        ] {
            assert_eq!(
                scheme_for(host, true),
                "https",
                "`{host}` was downgraded to plaintext; the variable must only ever \
                 reach a loopback literal, or a presigned url leaks in the clear"
            );
        }
    }

    #[test]
    fn a_port_is_stripped_without_mangling_an_ipv6_literal() {
        assert_eq!(host_without_port("127.0.0.1:9000"), "127.0.0.1");
        assert_eq!(
            host_without_port("minio.host.example"),
            "minio.host.example"
        );
        assert_eq!(host_without_port("[::1]:9000"), "::1");
        assert_eq!(host_without_port("[2001:db8::1]"), "2001:db8::1");
        // Bare IPv6, no brackets: taken whole rather than truncated at the last
        // colon, which would leave `::` and read as a different address.
        assert_eq!(host_without_port("::1"), "::1");
    }

    #[test]
    fn a_ledger_url_is_a_host_a_bucket_and_maybe_a_prefix() {
        assert_eq!(
            parse("s3://minio.host.example/quipu").unwrap(),
            Location {
                host: "minio.host.example".into(),
                bucket: "quipu".into(),
                prefix: String::new(),
            }
        );
        assert_eq!(
            parse("s3://minio.host.example:9000/quipu/ledgers/one").unwrap(),
            Location {
                host: "minio.host.example:9000".into(),
                bucket: "quipu".into(),
                prefix: "ledgers/one".into(),
            }
        );
    }

    #[test]
    fn anything_that_is_not_an_object_url_is_refused() {
        for bad in [
            "https://minio.host.example/quipu",
            "ssh://host.example/srv/ledger.git",
            "/srv/git/ledger.git",
            "s3://",
            "s3://minio.host.example",
            "s3://minio.host.example/",
            "s3:///quipu",
        ] {
            assert!(parse(bad).is_err(), "`{bad}` was accepted as a ledger url");
        }
    }

    #[test]
    fn a_project_key_survives_the_round_trip_into_a_key_segment() {
        for key in [
            "9f1c-plain",
            "legacy-9f1c",
            "with space",
            "../other/project",
            "sl/ash",
        ] {
            assert_eq!(decode_segment(&encode_segment(key)), key);
        }
    }

    #[test]
    fn a_project_key_can_never_escape_its_own_prefix() {
        let sneaky = "../other/project";
        let key = object_key(sneaky, "store-1", "b.jsonl");
        assert!(
            key.starts_with(&project_prefix(sneaky)),
            "{key} left its project's prefix"
        );
        assert_eq!(key.matches('/').count(), 2, "{key} has extra segments");
        assert_ne!(project_prefix(sneaky), project_prefix("other/project"));
    }

    #[test]
    fn only_a_batch_key_has_a_tail() {
        assert_eq!(
            key_tail("proj/store-1/store-1-0000000001-0000000009.jsonl"),
            Some((
                "store-1".to_string(),
                "store-1-0000000001-0000000009.jsonl".to_string()
            ))
        );
        assert_eq!(key_tail("proj/store-1/"), None);
        assert_eq!(key_tail("proj/README"), None);
        assert_eq!(key_tail("proj/store-1/nested/b.jsonl"), None);
    }

    #[test]
    fn an_s3_error_body_names_its_own_cause() {
        let body = "<?xml version=\"1.0\"?><Error><Code>NoSuchBucket</Code>\
                    <Message>The specified bucket does not exist</Message></Error>";
        assert_eq!(xml_field(body, "Code").as_deref(), Some("NoSuchBucket"));
        assert_eq!(
            xml_field(body, "Message").as_deref(),
            Some("The specified bucket does not exist")
        );
        assert_eq!(xml_field(body, "Absent"), None);
        assert!(mentions_time(Some(
            "Signature expired: 20240101T000000Z is now earlier"
        )));
        assert!(!mentions_time(Some("Access Denied")));
    }
}
