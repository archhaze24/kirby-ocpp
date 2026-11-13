//! A small HTTP/1.1 client for the control API: one request per connection, plain or TLS,
//! bounded heads and bodies. It speaks only what the API answers: `Content-Length`,
//! `chunked` (event streams) and, for the empty answers hyper itself produces, close-delimited
//! bodies.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use url::{Host, Url};

/// Largest non-streaming answer that is read: a station history is bounded by 8 MiB of
/// frames, and JSON escaping can grow it.
pub const MAX_BODY: usize = 64 * 1024 * 1024;
/// Largest response head (status line and headers).
const MAX_HEAD: usize = 64 * 1024;
/// Largest single header, status or chunk-size line.
const MAX_LINE: usize = 16 * 1024;
const MAX_HEADERS: usize = 100;
/// Piece size of body reads.
const PIECE: usize = 64 * 1024;

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

type Stream = BufReader<Box<dyn Io>>;

/// Where the API is: the parsed `--url`.
#[derive(Debug, Clone)]
pub struct Endpoint {
    tls: bool,
    connect_host: String,
    port: u16,
    host_header: String,
    /// Path prefix of a reverse proxy, without a trailing slash (empty by default).
    prefix: String,
    /// `scheme://host[:port]`, for messages.
    display: String,
    server_name: ServerName<'static>,
    loopback: bool,
}

impl Endpoint {
    pub fn parse(text: &str) -> Result<Self, String> {
        let url = Url::parse(text).map_err(|error| format!("invalid --url: {error}"))?;
        let tls = match url.scheme() {
            "http" => false,
            "https" => true,
            other => return Err(format!("--url must be http:// or https://, not {other}://")),
        };
        if !url.username().is_empty() || url.password().is_some() {
            return Err(
                "--url must not contain credentials; pass the token with --token-file or \
                 KIRBY_CSMS_API_TOKEN"
                    .to_owned(),
            );
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err("--url must not contain a query or a fragment".to_owned());
        }
        let host = url.host().ok_or("--url needs a host")?;
        let port = url.port_or_known_default().ok_or("--url needs a port")?;
        let (connect_host, host_text, server_name, loopback) = match host {
            Host::Domain(name) => {
                let server_name = ServerName::try_from(name.to_owned())
                    .map_err(|_| "--url: the host is not a valid TLS server name".to_owned())?;
                let loopback = name.eq_ignore_ascii_case("localhost")
                    || name.eq_ignore_ascii_case("localhost.");
                (name.to_owned(), name.to_owned(), server_name, loopback)
            }
            Host::Ipv4(ip) => (
                ip.to_string(),
                ip.to_string(),
                ServerName::IpAddress(ip.into()),
                ip.is_loopback(),
            ),
            Host::Ipv6(ip) => (
                ip.to_string(),
                format!("[{ip}]"),
                ServerName::IpAddress(ip.into()),
                ip.is_loopback(),
            ),
        };
        let host_header = match url.port() {
            Some(port) => format!("{host_text}:{port}"),
            None => host_text.clone(),
        };
        let display = format!("{}://{host_header}", if tls { "https" } else { "http" });
        Ok(Self {
            tls,
            connect_host,
            port,
            host_header,
            prefix: url.path().trim_end_matches('/').to_owned(),
            display,
            server_name,
            loopback,
        })
    }

    pub fn is_tls(&self) -> bool {
        self.tls
    }

    pub fn is_loopback(&self) -> bool {
        self.loopback
    }

    pub fn display(&self) -> &str {
        &self.display
    }
}

/// The TLS files of the client.
#[derive(Debug, Clone, Default)]
pub struct TlsFiles {
    pub ca: Option<PathBuf>,
    pub cert: Option<PathBuf>,
    pub key: Option<PathBuf>,
}

fn read_file(path: &std::path::Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

/// The rustls client configuration: the roots of `--ca-file` (or the system roots) and the
/// optional client identity.
fn tls_config(files: &TlsFiles) -> Result<Arc<ClientConfig>, String> {
    let mut roots = RootCertStore::empty();
    if let Some(path) = &files.ca {
        let bytes = read_file(path)?;
        for cert in CertificateDer::pem_slice_iter(&bytes) {
            let cert = cert.map_err(|e| format!("{}: {e}", path.display()))?;
            roots
                .add(cert)
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        if roots.is_empty() {
            return Err(format!("{} holds no certificate", path.display()));
        }
    } else {
        let native = rustls_native_certs::load_native_certs();
        roots.add_parsable_certificates(native.certs);
        if roots.is_empty() {
            return Err(
                "no system root certificates found; trust the API certificate with \
                        --ca-file"
                    .to_owned(),
            );
        }
    }
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots);
    let config = match (&files.cert, &files.key) {
        (Some(cert), Some(key)) => {
            let cert_bytes = read_file(cert)?;
            let chain = CertificateDer::pem_slice_iter(&cert_bytes)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("{}: {e}", cert.display()))?;
            if chain.is_empty() {
                return Err(format!("{} holds no certificate", cert.display()));
            }
            let key_bytes = read_file(key)?;
            let key_der = PrivateKeyDer::from_pem_slice(&key_bytes)
                .map_err(|e| format!("{}: {e}", key.display()))?;
            builder
                .with_client_auth_cert(chain, key_der)
                .map_err(|e| format!("client certificate: {e}"))?
        }
        _ => builder.with_no_client_auth(),
    };
    Ok(Arc::new(config))
}

/// A request line target and body.
#[derive(Debug)]
pub struct Request<'a> {
    pub method: &'a str,
    /// Path and query, already percent-encoded, without the endpoint prefix.
    pub path: &'a str,
    pub body: Option<&'a [u8]>,
    /// Extra headers; the values must be plain ASCII without control characters.
    pub headers: &'a [(&'a str, &'a str)],
}

/// Status and headers of an answer. Header names are lowercase.
#[derive(Debug)]
pub struct Head {
    pub status: u16,
    headers: Vec<(String, String)>,
}

impl Head {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// A complete answer.
#[derive(Debug)]
pub struct Reply {
    pub head: Head,
    pub body: Vec<u8>,
}

/// The API client: endpoint, token, TLS and the default deadline.
pub struct Client {
    endpoint: Endpoint,
    token: Option<String>,
    tls: Option<TlsConnector>,
    pub timeout: Duration,
}

impl Client {
    pub fn new(
        endpoint: Endpoint,
        token: Option<String>,
        files: &TlsFiles,
        timeout: Duration,
    ) -> Result<Self, String> {
        let tls = if endpoint.tls {
            Some(TlsConnector::from(tls_config(files)?))
        } else {
            None
        };
        Ok(Self {
            endpoint,
            token,
            tls,
            timeout,
        })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    async fn connect(&self) -> Result<Stream, String> {
        let display = &self.endpoint.display;
        let tcp = TcpStream::connect((self.endpoint.connect_host.as_str(), self.endpoint.port))
            .await
            .map_err(|error| format!("cannot connect to {display}: {error}"))?;
        let _ = tcp.set_nodelay(true);
        let io: Box<dyn Io> = match &self.tls {
            Some(connector) => {
                let tls = connector
                    .connect(self.endpoint.server_name.clone(), tcp)
                    .await
                    .map_err(|error| format!("TLS handshake with {display} failed: {error}"))?;
                Box::new(tls)
            }
            None => Box::new(tcp),
        };
        Ok(BufReader::new(io))
    }

    fn head_text(&self, request: &Request<'_>) -> String {
        let mut text = format!(
            "{} {}{} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: */*\r\n\
             User-Agent: kirby-csms-ctl/{}\r\n",
            request.method,
            self.endpoint.prefix,
            request.path,
            self.endpoint.host_header,
            env!("CARGO_PKG_VERSION"),
        );
        if let Some(token) = &self.token {
            let _ = write!(text, "Authorization: Bearer {token}\r\n");
        }
        for (name, value) in request.headers {
            let _ = write!(text, "{name}: {value}\r\n");
        }
        if let Some(body) = request.body {
            let _ = write!(
                text,
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                body.len()
            );
        }
        text.push_str("\r\n");
        text
    }

    /// Sends the request and reads the head; the body is read from the returned [`Body`].
    /// A failed write still tries to read an early answer (`401`, `413`).
    pub async fn open(&self, request: &Request<'_>) -> Result<(Head, Body), String> {
        let mut stream = self.connect().await?;
        let mut bytes = self.head_text(request).into_bytes();
        if let Some(body) = request.body {
            bytes.extend_from_slice(body);
        }
        let wrote = async {
            stream.write_all(&bytes).await?;
            stream.flush().await
        }
        .await;
        let head = match read_head(&mut stream).await {
            Ok(head) => head,
            Err(error) => {
                return Err(match wrote {
                    Err(write_error) => format!("cannot send the request: {write_error}"),
                    Ok(()) => error,
                });
            }
        };
        let framing = framing_of(request.method, &head)?;
        Ok((head, Body { stream, framing }))
    }

    /// A whole answer within `deadline` (connect, send and read).
    pub async fn send(&self, request: &Request<'_>, deadline: Duration) -> Result<Reply, String> {
        let work = async {
            let (head, mut body) = self.open(request).await?;
            let body = body.collect(MAX_BODY).await?;
            Ok(Reply { head, body })
        };
        tokio::time::timeout(deadline, work).await.map_err(|_| {
            format!(
                "no complete answer from {} within {} s (raise --timeout)",
                self.endpoint.display,
                deadline.as_secs()
            )
        })?
    }
}

/// Reads one line without its line end. `None` at a clean end of the stream. A TLS close
/// without `close_notify` counts as an end of the stream.
async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Option<Vec<u8>>, String> {
    let mut line = Vec::new();
    loop {
        let (used, found) = {
            let available = match reader.fill_buf().await {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => &[],
                Err(e) => return Err(format!("read failed: {e}")),
            };
            if available.is_empty() {
                return if line.is_empty() {
                    Ok(None)
                } else {
                    Err("the connection closed inside a line".to_owned())
                };
            }
            if let Some(at) = available.iter().position(|&byte| byte == b'\n') {
                line.extend_from_slice(&available[..at]);
                (at + 1, true)
            } else {
                line.extend_from_slice(available);
                (available.len(), false)
            }
        };
        reader.consume(used);
        if line.len() > max {
            return Err("a line of the answer is too long".to_owned());
        }
        if found {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
    }
}

async fn read_head(stream: &mut Stream) -> Result<Head, String> {
    let status_line = read_line(stream, MAX_LINE)
        .await?
        .ok_or("the connection closed before any answer")?;
    let status_line = String::from_utf8_lossy(&status_line).into_owned();
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    let code = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") || code.len() != 3 {
        return Err("the answer is not HTTP/1.x; is --url the control API?".to_owned());
    }
    let status: u16 = code
        .parse()
        .map_err(|_| "the answer has no valid status code".to_owned())?;
    if status < 200 {
        return Err(format!("unexpected interim status {status}"));
    }
    let mut headers = Vec::new();
    let mut size = status_line.len();
    loop {
        let line = read_line(stream, MAX_LINE)
            .await?
            .ok_or("the connection closed inside the headers")?;
        if line.is_empty() {
            break;
        }
        size += line.len();
        if size > MAX_HEAD || headers.len() >= MAX_HEADERS {
            return Err("the answer head is too large".to_owned());
        }
        let line = String::from_utf8_lossy(&line).into_owned();
        let (name, value) = line
            .split_once(':')
            .ok_or("the answer has a malformed header")?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    Ok(Head { status, headers })
}

#[derive(Debug)]
enum Framing {
    /// Bytes left.
    Length(u64),
    /// Bytes left in the current chunk and whether the last chunk was read.
    Chunked(u64, bool),
    /// Until the connection closes.
    Close,
}

fn framing_of(method: &str, head: &Head) -> Result<Framing, String> {
    if method == "HEAD" || head.status == 204 || head.status == 304 {
        return Ok(Framing::Length(0));
    }
    let chunked = head
        .headers
        .iter()
        .filter(|(name, _)| name == "transfer-encoding")
        .any(|(_, value)| value.to_ascii_lowercase().contains("chunked"));
    if chunked {
        return Ok(Framing::Chunked(0, false));
    }
    let mut length = None;
    for (name, value) in &head.headers {
        if name != "content-length" {
            continue;
        }
        let parsed: u64 = value
            .parse()
            .map_err(|_| "the answer has an invalid Content-Length".to_owned())?;
        if length.is_some_and(|known| known != parsed) {
            return Err("the answer has conflicting Content-Length headers".to_owned());
        }
        length = Some(parsed);
    }
    Ok(length.map_or(Framing::Close, Framing::Length))
}

/// The body of an answer, read piece by piece.
pub struct Body {
    stream: Stream,
    framing: Framing,
}

/// Up to `max` bytes of the stream; an end of the stream is an error.
async fn read_some(stream: &mut Stream, max: usize) -> Result<Vec<u8>, String> {
    let piece = {
        let available = match stream.fill_buf().await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => &[],
            Err(e) => return Err(format!("read failed: {e}")),
        };
        if available.is_empty() {
            return Err("the connection closed before the answer was complete".to_owned());
        }
        available[..available.len().min(max)].to_vec()
    };
    stream.consume(piece.len());
    Ok(piece)
}

fn chunk_size(line: &[u8]) -> Result<u64, String> {
    let text = String::from_utf8_lossy(line);
    let digits = text.split(';').next().unwrap_or_default().trim();
    if digits.is_empty() || digits.len() > 15 {
        return Err("the answer has an invalid chunk size".to_owned());
    }
    u64::from_str_radix(digits, 16).map_err(|_| "the answer has an invalid chunk size".to_owned())
}

impl Body {
    /// The next piece of the body, `None` at its end.
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        let Self { stream, framing } = self;
        match framing {
            Framing::Length(left) => {
                if *left == 0 {
                    return Ok(None);
                }
                let max = usize::try_from(*left).unwrap_or(usize::MAX).min(PIECE);
                let piece = read_some(stream, max).await?;
                *left -= piece.len() as u64;
                Ok(Some(piece))
            }
            Framing::Close => {
                let available = match stream.fill_buf().await {
                    Ok(bytes) => bytes,
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => &[],
                    Err(e) => return Err(format!("read failed: {e}")),
                };
                if available.is_empty() {
                    return Ok(None);
                }
                let piece = available[..available.len().min(PIECE)].to_vec();
                stream.consume(piece.len());
                Ok(Some(piece))
            }
            Framing::Chunked(left, done) => {
                if *done {
                    return Ok(None);
                }
                if *left == 0 {
                    let line = read_line(stream, MAX_LINE)
                        .await?
                        .ok_or("the connection closed inside a chunked answer")?;
                    let size = chunk_size(&line)?;
                    if size == 0 {
                        loop {
                            let trailer = read_line(stream, MAX_LINE)
                                .await?
                                .ok_or("the connection closed inside a chunked answer")?;
                            if trailer.is_empty() {
                                break;
                            }
                        }
                        *done = true;
                        return Ok(None);
                    }
                    *left = size;
                }
                let max = usize::try_from(*left).unwrap_or(usize::MAX).min(PIECE);
                let piece = read_some(stream, max).await?;
                *left -= piece.len() as u64;
                if *left == 0 {
                    let end = read_line(stream, MAX_LINE)
                        .await?
                        .ok_or("the connection closed inside a chunked answer")?;
                    if !end.is_empty() {
                        return Err("the answer has a malformed chunk".to_owned());
                    }
                }
                Ok(Some(piece))
            }
        }
    }

    /// The rest of the body, at most `cap` bytes.
    pub async fn collect(&mut self, cap: usize) -> Result<Vec<u8>, String> {
        let mut all = Vec::new();
        while let Some(piece) = self.next().await? {
            if all.len() + piece.len() > cap {
                return Err(format!(
                    "the answer is larger than {} MiB",
                    cap / (1024 * 1024)
                ));
            }
            all.extend_from_slice(&piece);
        }
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn endpoints_are_parsed_and_checked() {
        let plain = Endpoint::parse("http://127.0.0.1:9100").unwrap();
        assert_eq!(plain.host_header, "127.0.0.1:9100");
        assert_eq!(plain.prefix, "");
        assert!(plain.is_loopback() && !plain.is_tls());
        let v6 = Endpoint::parse("https://[::1]:9443/api/").unwrap();
        assert_eq!(v6.host_header, "[::1]:9443");
        assert_eq!(v6.connect_host, "::1");
        assert_eq!(v6.prefix, "/api");
        assert!(v6.is_tls() && v6.is_loopback());
        let named = Endpoint::parse("https://csms.example").unwrap();
        assert_eq!(named.host_header, "csms.example");
        assert_eq!(named.port, 443);
        assert!(!named.is_loopback());
        assert!(Endpoint::parse("http://LOCALHOST:1").unwrap().is_loopback());
        for bad in [
            "ftp://x",
            "http://user:secret@host",
            "http://user@host",
            "http://host/?a=b",
            "http://host/#x",
            "host",
            "http://",
        ] {
            let error = Endpoint::parse(bad).unwrap_err();
            assert!(!error.contains("secret"), "{error}");
        }
    }

    #[test]
    fn chunk_sizes_are_hex_and_bounded() {
        assert_eq!(chunk_size(b"1f").unwrap(), 31);
        assert_eq!(chunk_size(b"A;ext=1").unwrap(), 10);
        assert!(chunk_size(b"").is_err());
        assert!(chunk_size(b"zz").is_err());
        assert!(chunk_size(b"ffffffffffffffffff").is_err());
    }

    /// Serves `answer` to one connection and returns the endpoint.
    async fn serve(answer: &'static [u8]) -> Client {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buffer).await;
            socket.write_all(answer).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        let endpoint = Endpoint::parse(&format!("http://127.0.0.1:{port}")).unwrap();
        Client::new(
            endpoint,
            None,
            &TlsFiles::default(),
            Duration::from_secs(10),
        )
        .unwrap()
    }

    async fn get(client: &Client) -> Result<Reply, String> {
        let request = Request {
            method: "GET",
            path: "/v1/health",
            body: None,
            headers: &[],
        };
        client.send(&request, Duration::from_secs(10)).await
    }

    #[tokio::test]
    async fn bodies_are_framed_by_length_chunks_or_close() {
        let client = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello").await;
        assert_eq!(get(&client).await.unwrap().body, b"hello");
        let client = serve(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n4;x=y\r\ndefg\r\n0\r\nTrailer: 1\r\n\r\n",
        )
        .await;
        assert_eq!(get(&client).await.unwrap().body, b"abcdefg");
        let client = serve(b"HTTP/1.1 400 Bad Request\r\n\r\nrest of it").await;
        let reply = get(&client).await.unwrap();
        assert_eq!(reply.head.status, 400);
        assert_eq!(reply.body, b"rest of it");
    }

    #[tokio::test]
    async fn malformed_and_truncated_answers_are_errors() {
        for answer in [
            &b"SMTP 220 hello\r\n\r\n"[..],
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort",
            b"HTTP/1.1 200 OK\r\nContent-Length: x\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabcXX\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nab",
            b"",
        ] {
            let client = serve(answer).await;
            assert!(get(&client).await.is_err(), "{answer:?}");
        }
    }

    #[tokio::test]
    async fn the_body_cap_is_enforced() {
        let client = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789").await;
        let (_, mut body) = client
            .open(&Request {
                method: "GET",
                path: "/",
                body: None,
                headers: &[],
            })
            .await
            .unwrap();
        assert!(body.collect(4).await.is_err());
    }
}
