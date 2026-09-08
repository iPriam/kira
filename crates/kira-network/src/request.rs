//! Requests a caller builds a piece at a time, and the responses they read.
//!
//! The loopback operations beside this module take a port and answer with a
//! number, which is all a protocol demonstration needs. A program calling a real
//! service needs to say which method, which URL, which headers and which body,
//! and then to read what came back — and the C ABI it says all of that through
//! carries scalars and NUL-terminated strings, nothing else.
//!
//! So a request is assembled against a handle: `new` opens one, the setters fill
//! it, and `send` consumes it and hands back an ordinary operation handle that
//! polls, results and cancels exactly like every other. The response is read
//! back through the same handle, from a cursor the operation owns, because text
//! has no owned representation at a C boundary that returns `int64_t`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderName, HeaderValue, Method};

use crate::api::{HttpClient, HttpClientConfig, HttpRequest, HttpResponse, HttpVersion};
use crate::runtime::{self, NetworkError, OperationId};

/// The value the response readers return at the end of a selection, and that
/// `select_header` returns for a header the response does not carry.
///
/// Public because it is part of the C ABI rather than of this module: a caller
/// in any language compares against it, and the header defines the same number
/// as `KIRA_NETWORK_END_OF_SELECTION`.
pub const END_OF_SELECTION: i64 = -1;

/// A request being assembled through the C surface.
#[derive(Debug)]
struct RequestSpec {
    method: Method,
    url: String,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Vec<u8>,
    version: HttpVersion,
    timeout: Option<Duration>,
    roots: Vec<Vec<u8>>,
}

impl RequestSpec {
    /// Turns the assembled parts into the request the client sends.
    fn into_request(self) -> Result<HttpRequest, NetworkError> {
        let mut request =
            HttpRequest::new(self.method, &self.url)?.with_body(Bytes::from(self.body));
        let mut named_agent = false;
        for (name, value) in self.headers {
            named_agent |= name == http::header::USER_AGENT;
            request = request.with_header(name, value);
        }
        // A server that logs or rate-limits by client has to have something to
        // name. Only when the caller named none of its own: whoever sets the
        // header meant it.
        if !named_agent {
            let agent = concat!("kira-network/", env!("CARGO_PKG_VERSION"));
            request =
                request.with_header(http::header::USER_AGENT, HeaderValue::from_static(agent));
        }
        Ok(request)
    }
}

/// The requests currently under construction.
struct RequestTable {
    next_id: AtomicU64,
    specs: Mutex<HashMap<i64, RequestSpec>>,
}

static REQUESTS: OnceLock<RequestTable> = OnceLock::new();

fn requests() -> &'static RequestTable {
    REQUESTS.get_or_init(|| RequestTable {
        next_id: AtomicU64::new(1),
        specs: Mutex::new(HashMap::new()),
    })
}

/// Opens a request for `method` and `url`.
pub(crate) fn new(method: &str, url: &str) -> Result<i64, NetworkError> {
    let method = Method::try_from(method).map_err(|_| NetworkError::InvalidConfig)?;
    // Parsed here rather than at `send`, so a typo is reported by the call that
    // contains it instead of by an operation started minutes later.
    let parsed: http::Uri = url.parse().map_err(|_| NetworkError::InvalidUri)?;
    match parsed.scheme_str() {
        Some("http" | "https") => {}
        _ => return Err(NetworkError::InvalidUri),
    }
    if parsed.authority().is_none() {
        return Err(NetworkError::InvalidUri);
    }
    let table = requests();
    let id = table.next_id.fetch_add(1, Ordering::Relaxed);
    if id == 0 || id > i64::MAX as u64 {
        return Err(NetworkError::IdExhausted);
    }
    let id = id as i64;
    let spec = RequestSpec {
        method,
        url: url.to_owned(),
        headers: Vec::new(),
        body: Vec::new(),
        version: HttpVersion::Http1,
        timeout: Some(Duration::from_secs(30)),
        roots: Vec::new(),
    };
    table
        .specs
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?
        .insert(id, spec);
    Ok(id)
}

/// Runs `action` against a request still under construction.
fn with_spec<T>(
    id: i64,
    action: impl FnOnce(&mut RequestSpec) -> Result<T, NetworkError>,
) -> Result<T, NetworkError> {
    let mut specs = requests()
        .specs
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?;
    let spec = specs.get_mut(&id).ok_or(NetworkError::UnknownHandle)?;
    action(spec)
}

/// Adds one header to a request.
pub(crate) fn add_header(id: i64, name: &str, value: &str) -> Result<(), NetworkError> {
    let name = HeaderName::try_from(name).map_err(|_| NetworkError::Header)?;
    let value = HeaderValue::try_from(value).map_err(|_| NetworkError::Header)?;
    with_spec(id, |spec| {
        spec.headers.push((name, value));
        Ok(())
    })
}

/// Replaces a request's body with the bytes of `text`.
pub(crate) fn set_body_text(id: i64, text: &str) -> Result<(), NetworkError> {
    with_spec(id, |spec| {
        spec.body = text.as_bytes().to_vec();
        Ok(())
    })
}

/// Appends one byte to a request's body, for a body that is not text.
pub(crate) fn push_body_byte(id: i64, byte: i32) -> Result<(), NetworkError> {
    let byte = u8::try_from(byte).map_err(|_| NetworkError::InvalidConfig)?;
    with_spec(id, |spec| {
        spec.body.push(byte);
        Ok(())
    })
}

/// Sets the deadline for the whole request, or clears it with zero.
pub(crate) fn set_timeout_ms(id: i64, milliseconds: i64) -> Result<(), NetworkError> {
    let timeout = match milliseconds {
        0 => None,
        value if value > 0 => Some(Duration::from_millis(value as u64)),
        _ => return Err(NetworkError::InvalidConfig),
    };
    with_spec(id, |spec| {
        spec.timeout = timeout;
        Ok(())
    })
}

/// Selects the HTTP version: `1` for HTTP/1.1, `2` for HTTP/2.
pub(crate) fn set_version(id: i64, version: i32) -> Result<(), NetworkError> {
    let version = match version {
        1 => HttpVersion::Http1,
        2 => HttpVersion::Http2,
        _ => return Err(NetworkError::InvalidConfig),
    };
    with_spec(id, |spec| {
        spec.version = version;
        Ok(())
    })
}

/// Trusts the certificate published by the loopback server on `port`.
///
/// The public roots stay in place: this adds one anchor for a server that
/// generated its own certificate in this process, which is what makes an
/// end-to-end TLS test possible on a machine with no internet and no CA.
pub(crate) fn trust_loopback(id: i64, port: u16) -> Result<(), NetworkError> {
    let certificate = runtime::server_certificate(port)?;
    with_spec(id, |spec| {
        spec.roots.push(certificate.to_vec());
        Ok(())
    })
}

/// Discards a request that will never be sent.
pub(crate) fn discard(id: i64) {
    if let Ok(mut specs) = requests().specs.lock() {
        specs.remove(&id);
    }
}

/// Sends an assembled request and returns its operation handle.
pub(crate) fn send(id: i64) -> Result<OperationId, NetworkError> {
    let spec = requests()
        .specs
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?
        .remove(&id)
        .ok_or(NetworkError::UnknownHandle)?;
    let client = client_for(spec.version, &spec.roots)?;
    let timeout = spec.timeout;
    let request = spec.into_request()?;
    runtime::register_request(async move {
        // Both halves under one deadline, because `set_timeout_ms` documents a
        // whole-request one. A server that sends headers and then stalls ends
        // `client.request` promptly and leaves the body unread, so a deadline
        // that covered only the send would let that peer hold a Kira caller
        // open for as long as it kept the connection alive.
        let whole = async move {
            let response = client.request(request).await?;
            ResponseData::collect(response).await
        };
        match timeout {
            Some(limit) => tokio::time::timeout(limit, whole)
                .await
                .map_err(|_| NetworkError::Timeout)?,
            None => whole.await,
        }
    })
}

/// The pooled clients, one per version and trust configuration.
///
/// Keyed rather than rebuilt per request because a client owns the connection
/// pool: a new one for every call would hand back a fresh TCP connection and a
/// fresh TLS handshake each time, and the pooling the client implements would
/// never once be used.
/// What distinguishes one pooled client from another: two requests share a
/// client exactly when they would negotiate the same protocol against the same
/// set of trust anchors.
type ClientKey = (HttpVersion, Vec<Vec<u8>>);

static CLIENTS: OnceLock<Mutex<Vec<(ClientKey, HttpClient)>>> = OnceLock::new();

/// How many pooled clients carrying caller-supplied trust anchors are kept.
///
/// The default-trust clients are few — one per protocol version — but a
/// caller-supplied anchor set is unbounded in a way they are not: every
/// loopback HTTPS server started by a test presents a fresh certificate, so a
/// start/trust/send/close cycle mints a distinct key each time it runs. Kept
/// unbounded, each retained client holds a connection pool and its anchor
/// buffers for the life of the process.
///
/// Bounded and evicted oldest-first rather than not cached at all, so a program
/// that pins one corporate root still gets the pooling this cache exists for.
const MAX_CUSTOM_ROOT_CLIENTS: usize = 8;

fn client_for(version: HttpVersion, roots: &[Vec<u8>]) -> Result<HttpClient, NetworkError> {
    let clients = CLIENTS.get_or_init(|| Mutex::new(Vec::new()));
    let mut clients = clients.lock().map_err(|_| NetworkError::RuntimeInit)?;
    if let Some((_, client)) = clients
        .iter()
        .find(|((cached, cached_roots), _)| *cached == version && cached_roots == roots)
    {
        return Ok(client.clone());
    }
    if !roots.is_empty() {
        let custom = clients
            .iter()
            .filter(|((_, cached_roots), _)| !cached_roots.is_empty())
            .count();
        if custom >= MAX_CUSTOM_ROOT_CLIENTS
            && let Some(oldest) = clients
                .iter()
                .position(|((_, cached_roots), _)| !cached_roots.is_empty())
        {
            clients.remove(oldest);
        }
    }
    let client = HttpClient::new(HttpClientConfig {
        version,
        // The deadline belongs to the request rather than to the client the
        // request happened to share, so it is applied around the send.
        request_timeout: None,
        root_certificates: roots.to_vec(),
        ..HttpClientConfig::default()
    })?;
    clients.push(((version, roots.to_vec()), client.clone()));
    Ok(client)
}

/// A response held for reading after its operation completed.
#[derive(Debug)]
pub(crate) struct ResponseData {
    status: u16,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
}

impl ResponseData {
    /// Reads a completed response into owned storage.
    async fn collect(response: HttpResponse) -> Result<Self, NetworkError> {
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
            .collect();
        let body = response.bytes().await?.to_vec();
        Ok(Self {
            status,
            headers,
            body,
        })
    }

    /// The HTTP status code.
    pub(crate) fn status(&self) -> u16 {
        self.status
    }

    /// The bytes the cursor is reading.
    fn selection(&self, cursor: &Cursor) -> &[u8] {
        match cursor.source {
            Source::Body => &self.body,
            Source::Header(index) => self
                .headers
                .get(index)
                .map(|(_, value)| value.as_slice())
                .unwrap_or_default(),
            Source::Absent => &[],
        }
    }

    /// The index of the first header named `name`, case-insensitively.
    fn header_index(&self, name: &str) -> Option<usize> {
        self.headers
            .iter()
            .position(|(header, _)| header.eq_ignore_ascii_case(name))
    }
}

/// Which part of a response a handle is reading, and how far it has read.
#[derive(Debug, Default)]
pub(crate) struct Cursor {
    source: Source,
    offset: usize,
}

#[derive(Debug, Default, Clone, Copy)]
enum Source {
    #[default]
    Body,
    Header(usize),
    /// A header the response does not carry.
    ///
    /// Selecting nothing rather than leaving the previous selection in place:
    /// a caller that read past the `-1` would otherwise get bytes of whatever
    /// it had selected before, which is worse than reading nothing at all.
    Absent,
}

/// Selects the body and returns its length in bytes.
pub(crate) fn select_body(handle: OperationId) -> Result<i64, NetworkError> {
    runtime::with_response(handle, |response, cursor| {
        cursor.source = Source::Body;
        cursor.offset = 0;
        response.body.len() as i64
    })
}

/// Selects a header's value and returns its length, or `-1` when it is absent.
pub(crate) fn select_header(handle: OperationId, name: &str) -> Result<i64, NetworkError> {
    runtime::with_response(handle, |response, cursor| {
        let Some(index) = response.header_index(name) else {
            cursor.source = Source::Absent;
            cursor.offset = 0;
            return END_OF_SELECTION;
        };
        cursor.source = Source::Header(index);
        cursor.offset = 0;
        response.selection(cursor).len() as i64
    })
}

/// The length of the current selection, without moving the cursor.
pub(crate) fn selection_length(handle: OperationId) -> Result<i64, NetworkError> {
    runtime::with_response(handle, |response, cursor| {
        response.selection(cursor).len() as i64
    })
}

/// Moves the cursor back to the start of the current selection.
pub(crate) fn rewind(handle: OperationId) -> Result<i64, NetworkError> {
    runtime::with_response(handle, |_, cursor| {
        cursor.offset = 0;
        0
    })
}

/// Reads the next byte, or `-1` at the end of the selection.
pub(crate) fn read_byte(handle: OperationId) -> Result<i64, NetworkError> {
    runtime::with_response(handle, |response, cursor| {
        let bytes = response.selection(cursor);
        match bytes.get(cursor.offset) {
            Some(byte) => {
                cursor.offset += 1;
                i64::from(*byte)
            }
            None => END_OF_SELECTION,
        }
    })
}

/// Reads the next Unicode scalar, or `-1` at the end of the selection.
///
/// Scalars rather than bytes because the caller is rebuilding text: Kira's
/// `scalarText` takes a code point, so a byte-wise reader would make every
/// program that wanted the response as a `String` implement UTF-8 decoding
/// first, and each of them would be a place to get it wrong.
pub(crate) fn read_scalar(handle: OperationId) -> Result<i64, NetworkError> {
    runtime::with_response(handle, |response, cursor| {
        let bytes = response.selection(cursor);
        if cursor.offset >= bytes.len() {
            return Ok(END_OF_SELECTION);
        }
        let remaining = &bytes[cursor.offset..];
        let width = scalar_width(remaining[0])?;
        let scalar = remaining
            .get(..width)
            .and_then(|head| std::str::from_utf8(head).ok())
            .and_then(|text| text.chars().next())
            .ok_or(NetworkError::Encoding)?;
        cursor.offset += width;
        Ok(i64::from(u32::from(scalar)))
    })?
}

/// The length in bytes of the UTF-8 sequence a leading byte opens.
fn scalar_width(leading: u8) -> Result<usize, NetworkError> {
    match leading {
        0x00..=0x7f => Ok(1),
        0xc2..=0xdf => Ok(2),
        0xe0..=0xef => Ok(3),
        0xf0..=0xf4 => Ok(4),
        _ => Err(NetworkError::Encoding),
    }
}

/// The status code of a completed request.
pub(crate) fn status(handle: OperationId) -> Result<i64, NetworkError> {
    runtime::with_response(handle, |response, _| i64::from(response.status))
}
