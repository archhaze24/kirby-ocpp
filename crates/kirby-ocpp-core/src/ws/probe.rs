//! The connection doctor: a step-by-step check of the path to a Central System.
//!
//! [`run`] walks the steps `url`, `dns`, `tcp`, `tls`, `websocket`, `boot` and `close`, one
//! after another, and reports each as passed, warning, failed or skipped. A failed step
//! skips every step that depends on it. The probe opens one connection, sends exactly one
//! `BootNotification` and closes. It uses the same TLS configuration and the same upgrade
//! code as a station, but never touches the station state.
//!
//! The report is version-neutral in shape (`ocpp_version` names the protocol); the step
//! names are generic.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use url::{Host, Url};

use super::handshake::{self, UpgradeInfo};
use super::{
    ConnectError, ConnectOptions, Connection, ConnectionEvent, DEFAULT_WRITE_TIMEOUT, SchemaSet,
    task, tls,
};
use crate::config::{StationConfig, TlsConfig};
use crate::error::CallError;
use crate::ocpp::messages::{BootNotification, BootNotificationRequest};
use crate::ocpp::types::RegistrationStatus;
use crate::scenario::{OCPP_VERSION, SCHEMA_VERSION, TOOL_VERSION};
use crate::text::is_unsafe_char;

/// Names of the steps, in order.
pub const STEP_NAMES: [&str; 7] = ["url", "dns", "tcp", "tls", "websocket", "boot", "close"];

/// A server certificate that expires within this many days is reported as a warning.
pub const CERT_WARN_DAYS: i64 = 14;
/// A server clock that differs from ours by more than this many seconds is reported as a warning.
pub const SKEW_WARN_SECS: i64 = 60;
/// Default limit of one connection step.
pub const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest wait for the close handshake.
const CLOSE_WAIT: Duration = Duration::from_secs(5);
/// Most addresses listed in a detail line.
const MAX_LISTED_ADDRESSES: usize = 6;

/// Outcome of one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Passed,
    Warning,
    Failed,
    Skipped,
}

impl StepStatus {
    /// The label of the plain text output.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Passed => "PASS",
            Self::Warning => "WARN",
            Self::Failed => "FAIL",
            Self::Skipped => "SKIP",
        }
    }
}

/// One line of the checklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    pub name: &'static str,
    pub status: StepStatus,
    pub detail: String,
    /// What to try when the step did not pass; `null` otherwise.
    pub hint: Option<String>,
    pub duration_ms: u64,
}

/// The result of a check. Serialized as the `--json` output of `kirby-ocpp check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    pub schema: u32,
    pub ocpp_version: &'static str,
    pub tool_version: &'static str,
    /// The WebSocket URL that was checked; a password is replaced by `***`.
    pub url: String,
    /// `passed` unless a step failed.
    pub status: &'static str,
    pub steps: Vec<Step>,
}

impl Report {
    /// Whether no step failed (warnings are allowed).
    #[must_use]
    pub fn passed(&self) -> bool {
        self.status == "passed"
    }
}

/// What to check and how.
#[derive(Debug, Clone)]
pub struct ProbeOptions {
    /// The base URL of the Central System (the charge point id is appended).
    pub url: String,
    pub charge_point_id: String,
    pub subprotocol: String,
    pub tls: TlsConfig,
    pub vendor: String,
    pub model: String,
    pub serial_number: Option<String>,
    pub firmware_version: Option<String>,
    /// Limit of the whole check.
    pub timeout: Duration,
    /// Limit of one connection step (also bounded by what is left of `timeout`).
    pub step_timeout: Duration,
    /// Limit of the `BootNotification` answer (also bounded by what is left of `timeout`).
    pub call_timeout: Duration,
}

impl ProbeOptions {
    /// The options a station with `config` would connect with.
    #[must_use]
    pub fn from_config(config: &StationConfig, timeout: Duration) -> Self {
        Self {
            url: config.url.clone(),
            charge_point_id: config.charge_point_id.clone(),
            subprotocol: config.ws_subprotocol.clone(),
            tls: config.tls.clone(),
            vendor: config.vendor.clone(),
            model: config.model.clone(),
            serial_number: config.serial_number.clone(),
            firmware_version: config.firmware_version.clone(),
            timeout,
            step_timeout: DEFAULT_STEP_TIMEOUT,
            call_timeout: config.call_timeout(),
        }
    }
}

/// Facts about the server certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertFacts {
    pub subject: String,
    pub issuer: String,
    pub not_after: DateTime<Utc>,
}

/// Collects the steps and keeps the deadline.
struct Recorder<'a> {
    steps: Vec<Step>,
    on_step: &'a mut dyn FnMut(&Step),
    deadline: Instant,
    step_timeout: Duration,
    /// The first failed step.
    failed: Option<&'static str>,
}

impl Recorder<'_> {
    /// The time one more step may take.
    fn cap(&self) -> Duration {
        self.step_timeout
            .min(self.deadline.saturating_duration_since(Instant::now()))
    }

    fn push(
        &mut self,
        name: &'static str,
        started: Instant,
        status: StepStatus,
        detail: String,
        hint: Option<String>,
    ) {
        if status == StepStatus::Failed && self.failed.is_none() {
            self.failed = Some(name);
        }
        let step = Step {
            name,
            status,
            detail,
            hint,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        };
        (self.on_step)(&step);
        self.steps.push(step);
    }

    fn pass(&mut self, name: &'static str, started: Instant, detail: String) {
        self.push(name, started, StepStatus::Passed, detail, None);
    }

    fn fail(&mut self, name: &'static str, started: Instant, detail: String, hint: Option<String>) {
        self.push(name, started, StepStatus::Failed, detail, hint);
    }

    fn timed_out(&mut self, name: &'static str, started: Instant, what: &str) {
        let limit = started.elapsed();
        self.fail(
            name,
            started,
            format!(
                "{what} did not finish in time (gave up after {} ms)",
                limit.as_millis()
            ),
            Some("raise --timeout, or check that a firewall does not drop the packets".to_owned()),
        );
    }

    /// Adds a skipped line for every step that has none yet, and builds the report.
    fn finish(mut self, url: String) -> Report {
        for name in STEP_NAMES {
            if self.steps.iter().all(|step| step.name != name) {
                let detail = self.failed.map_or_else(
                    || "not run".to_owned(),
                    |failed| format!("skipped: the `{failed}` step failed"),
                );
                self.push(name, Instant::now(), StepStatus::Skipped, detail, None);
            }
        }
        let failed = self.steps.iter().any(|s| s.status == StepStatus::Failed);
        Report {
            schema: SCHEMA_VERSION,
            ocpp_version: OCPP_VERSION,
            tool_version: TOOL_VERSION,
            url,
            status: if failed { "failed" } else { "passed" },
            steps: self.steps,
        }
    }
}

/// Runs the whole check. `on_step` is called with each step as soon as it is known.
pub async fn run(options: &ProbeOptions, on_step: &mut dyn FnMut(&Step)) -> Report {
    let mut recorder = Recorder {
        steps: Vec::new(),
        on_step,
        deadline: Instant::now() + options.timeout,
        step_timeout: options.step_timeout,
        failed: None,
    };
    let shown = run_steps(options, &mut recorder).await;
    recorder.finish(shown)
}

/// The URL as printed: the password hidden, the user kept (it is the charge point id).
fn shown_url(url: &Url) -> String {
    let mut shown = url.clone();
    if shown.password().is_some() {
        let _ = shown.set_password(Some("***"));
    }
    // A query often carries a token: keep the names, hide the values.
    if shown.query().is_some() {
        let names: Vec<String> = shown
            .query_pairs()
            .map(|(name, _)| format!("{}=***", name.replace(['&', '#', '='], "")))
            .collect();
        shown.set_query(Some(&names.join("&")));
    }
    clean(shown.as_str(), 300)
}

/// Server- or user-controlled text made safe to print: unsafe characters dropped, length bounded.
fn clean(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !is_unsafe_char(*c))
        .take(max)
        .collect()
}

#[allow(clippy::too_many_lines, reason = "one linear checklist")]
async fn run_steps(o: &ProbeOptions, rec: &mut Recorder<'_>) -> String {
    // 1. URL
    let started = Instant::now();
    let url = match super::charge_point_url(&o.url, &o.charge_point_id) {
        Ok(url) => url,
        Err(error) => {
            rec.fail(
                "url",
                started,
                clean(&error.to_string(), 300),
                Some("use ws://host[:port]/path or wss://host[:port]/path".to_owned()),
            );
            return crate::config::redact_unparsable_url(&o.url);
        }
    };
    let shown = shown_url(&url);
    let wss = url.scheme() == "wss";
    let (Some(host), Some(port)) = (url.host(), url.port_or_known_default()) else {
        rec.fail("url", started, format!("{shown}: no host or port"), None);
        return shown;
    };
    rec.pass(
        "url",
        started,
        format!("{shown} (scheme {}, port {port})", url.scheme()),
    );

    // 2. DNS
    let started = Instant::now();
    let addrs: Vec<SocketAddr> = match host {
        Host::Ipv4(ip) => {
            rec.pass(
                "dns",
                started,
                format!("{ip} is an IP address, no lookup needed"),
            );
            vec![SocketAddr::new(ip.into(), port)]
        }
        Host::Ipv6(ip) => {
            rec.pass(
                "dns",
                started,
                format!("{ip} is an IP address, no lookup needed"),
            );
            vec![SocketAddr::new(ip.into(), port)]
        }
        Host::Domain(name) => {
            let name = name.to_owned();
            // A detached thread, not `spawn_blocking`: a resolver that never answers must not hold
            // up the runtime shutdown after the step has timed out.
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let lookup_name = name.clone();
            std::thread::spawn(move || {
                let found = std::net::ToSocketAddrs::to_socket_addrs(&(lookup_name.as_str(), port))
                    .map(Iterator::collect::<Vec<SocketAddr>>);
                let _ = sender.send(found);
            });
            let lookup = timeout(rec.cap(), receiver).await.map(|received| {
                received
                    .unwrap_or_else(|_| Err(std::io::Error::other("the resolver thread failed")))
            });
            match lookup {
                Ok(Ok(mut addrs)) => {
                    addrs.dedup();
                    if addrs.is_empty() {
                        rec.fail(
                            "dns",
                            started,
                            format!("{} has no addresses", clean(&name, 100)),
                            Some("check the host name".to_owned()),
                        );
                        return shown;
                    }
                    rec.pass(
                        "dns",
                        started,
                        format!("{} -> {}", clean(&name, 100), list_addresses(&addrs)),
                    );
                    addrs
                }
                Ok(Err(error)) => {
                    rec.fail(
                        "dns",
                        started,
                        format!("cannot resolve {}: {error}", clean(&name, 100)),
                        Some("check the host name and the DNS settings of this machine".to_owned()),
                    );
                    return shown;
                }
                Err(_) => {
                    rec.timed_out("dns", started, "the name lookup");
                    return shown;
                }
            }
        }
    };

    // 3. TCP
    let started = Instant::now();
    let mut errors: Vec<(String, std::io::ErrorKind)> = Vec::new();
    let mut connected = None;
    for addr in &addrs {
        match timeout(rec.cap(), TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                connected = Some((stream, *addr));
                break;
            }
            Ok(Err(error)) => errors.push((format!("{addr}: {error}"), error.kind())),
            Err(_) => errors.push((format!("{addr}: timed out"), std::io::ErrorKind::TimedOut)),
        }
    }
    let Some((tcp, addr)) = connected else {
        let kind = errors.first().map(|(_, kind)| *kind);
        let detail = errors
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        rec.fail("tcp", started, detail, tcp_hint(kind));
        return shown;
    };
    rec.pass(
        "tcp",
        started,
        format!(
            "connected to {addr} in {} ms",
            started.elapsed().as_millis()
        ),
    );

    // 4. TLS, then the session on the stream
    if !wss {
        rec.push(
            "tls",
            Instant::now(),
            StepStatus::Skipped,
            "not applicable: the URL scheme is ws".to_owned(),
            None,
        );
        session(tcp, &url, o, rec).await;
        return shown;
    }
    let started = Instant::now();
    let config = match tls::client_config(&o.tls).await {
        Ok(config) => config,
        Err(error) => {
            rec.fail(
                "tls",
                started,
                clean(&error.to_string(), 300),
                Some("check the --tls-ca, --tls-cert and --tls-key files".to_owned()),
            );
            return shown;
        }
    };
    let server_name = match tls::server_name(&url, &o.tls) {
        Ok(name) => name,
        Err(error) => {
            rec.fail("tls", started, clean(&error.to_string(), 300), None);
            return shown;
        }
    };
    match timeout(
        rec.cap(),
        TlsConnector::from(config).connect(server_name, tcp),
    )
    .await
    {
        Ok(Ok(stream)) => {
            let (version, cert) = tls_facts(&stream);
            let (status, detail) = describe_tls(&version, cert.as_ref(), &o.tls, Utc::now());
            rec.push("tls", started, status, detail, None);
            if status != StepStatus::Failed {
                session(stream, &url, o, rec).await;
            }
        }
        Ok(Err(error)) => {
            let mut detail = format!("TLS handshake failed: {}", clean(&error.to_string(), 300));
            let cert = if o.tls.skip_verify {
                None
            } else {
                describe_unverified(addr, &url, o, rec.cap()).await
            };
            if let Some(cert) = &cert {
                detail.push_str("; the server presents ");
                detail.push_str(&cert_summary(cert, Utc::now()));
            }
            let hint = tls_hint(&error.to_string(), o.tls.cert_file.is_some());
            rec.fail("tls", started, detail, hint);
        }
        Err(_) => rec.timed_out("tls", started, "the TLS handshake"),
    }
    shown
}

/// The steps after the transport is up: upgrade, `BootNotification`, close.
#[allow(clippy::too_many_lines, reason = "one linear checklist")]
async fn session<S>(stream: S, url: &Url, o: &ProbeOptions, rec: &mut Recorder<'_>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // 5. WebSocket upgrade
    let started = Instant::now();
    let mut info = UpgradeInfo::default();
    let upgraded = timeout(
        rec.cap(),
        handshake::upgrade_with_info(stream, url, &o.subprotocol, &mut info),
    )
    .await;
    let ws = match upgraded {
        Ok(Ok(ws)) => ws,
        Ok(Err(error)) => {
            let has_userinfo = !url.username().is_empty() || url.password().is_some();
            let (detail, hint) = upgrade_failure(&error, &info, &o.subprotocol, has_userinfo);
            rec.fail("websocket", started, detail, hint);
            return;
        }
        Err(_) => {
            rec.timed_out("websocket", started, "the WebSocket upgrade");
            return;
        }
    };
    rec.pass(
        "websocket",
        started,
        format!(
            "101 Switching Protocols, subprotocol `{}`",
            info.protocol.as_deref().unwrap_or(&o.subprotocol)
        ),
    );

    // 6. BootNotification
    let started = Instant::now();
    let options = ConnectOptions {
        ping_interval: None,
        call_timeout: o.call_timeout,
        serialize_calls: true,
        show_secrets: false,
        write_timeout: DEFAULT_WRITE_TIMEOUT,
        schemas: SchemaSet::default(),
    };
    let (open, commands, mut events) = task::spawn(ws, &options);
    let connection = Connection {
        commands,
        open,
        default_timeout: o.call_timeout,
        schemas: options.schemas.clone(),
    };
    // Nobody answers CSMS calls here: a dropped call gets a CALLERROR. The task only
    // reports when the socket is gone.
    let (closed_tx, closed_rx) = oneshot::channel();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let ConnectionEvent::Closed { code, by_us, .. } = event {
                let _ = closed_tx.send((code, by_us));
                break;
            }
        }
    });
    let request = BootNotificationRequest {
        charge_point_vendor: o.vendor.clone(),
        charge_point_model: o.model.clone(),
        charge_point_serial_number: o.serial_number.clone(),
        charge_box_serial_number: None,
        firmware_version: o.firmware_version.clone(),
        iccid: None,
        imsi: None,
        meter_type: None,
        meter_serial_number: None,
    };
    let wait = rec.cap().min(o.call_timeout);
    match connection
        .call_typed::<BootNotification>(&request, Some(wait))
        .await
    {
        Ok(response) => {
            let skew = (response.current_time - Utc::now()).num_seconds();
            let (status, detail, hint) = classify_boot(response.status, response.interval, skew);
            rec.push("boot", started, status, detail, hint);
        }
        Err(error) => {
            let (detail, hint) = boot_failure(&error);
            rec.fail("boot", started, detail, hint);
        }
    }

    // 7. Close
    let started = Instant::now();
    if !connection.is_open() {
        rec.push(
            "close",
            started,
            StepStatus::Skipped,
            "the connection was already closed".to_owned(),
            None,
        );
        return;
    }
    connection.close();
    // Bounded by what is left of the overall deadline, like every other step.
    let close_wait = rec.cap().min(CLOSE_WAIT);
    match timeout(close_wait, closed_rx).await {
        Ok(Ok((code, _))) => {
            let code_text = code.map_or_else(|| "no code".to_owned(), |c| c.to_string());
            if code == Some(1006) {
                // The handshake is incomplete, but the check itself is over: warn only.
                rec.push(
                    "close",
                    started,
                    StepStatus::Warning,
                    "closed by the station; the server ended the connection without a close reply"
                        .to_owned(),
                    None,
                );
            } else {
                rec.pass(
                    "close",
                    started,
                    format!("closed by the station (code {code_text})"),
                );
            }
        }
        _ => rec.push(
            "close",
            started,
            StepStatus::Warning,
            format!("no close reply within {} ms", close_wait.as_millis()),
            None,
        ),
    }
}

fn list_addresses(addrs: &[SocketAddr]) -> String {
    let mut text = addrs
        .iter()
        .take(MAX_LISTED_ADDRESSES)
        .map(|addr| addr.ip().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if addrs.len() > MAX_LISTED_ADDRESSES {
        let _ = write!(text, " (+{} more)", addrs.len() - MAX_LISTED_ADDRESSES);
    }
    text
}

/// What to try after a failed TCP connection.
#[must_use]
pub fn tcp_hint(kind: Option<std::io::ErrorKind>) -> Option<String> {
    use std::io::ErrorKind;
    match kind? {
        ErrorKind::ConnectionRefused => Some(
            "nothing listens on that address and port: check the port and that the Central System runs"
                .to_owned(),
        ),
        ErrorKind::TimedOut => Some(
            "no answer: a firewall may drop the packets, or the address is not reachable from here"
                .to_owned(),
        ),
        _ => Some("check the address and the network route to the Central System".to_owned()),
    }
}

/// What to try after a failed TLS handshake; `error` is the error text.
#[must_use]
pub fn tls_hint(error: &str, has_client_cert: bool) -> Option<String> {
    if error.contains("UnknownIssuer") {
        Some(
            "the certificate is not signed by a trusted CA: pass the CA with --tls-ca <FILE>, or use --tls-skip-verify for local testing"
                .to_owned(),
        )
    } else if error.contains("NotValidForName") {
        Some(
            "the certificate does not name this host: use the host name from the certificate in the URL, or set --tls-server-name"
                .to_owned(),
        )
    } else if error.contains("Expired") {
        Some("the certificate has expired: renew it on the Central System".to_owned())
    } else if error.contains("NotValidYet") {
        Some("the certificate is not valid yet: check the clock of this machine".to_owned())
    } else if error.contains("wildcard") {
        Some(
            "the Central System certificate names a wildcard, which this client refuses".to_owned(),
        )
    } else if error.contains("alert") || error.contains("CertificateRequired") {
        Some(if has_client_cert {
            "the server refused the handshake: check that it trusts the client certificate (--tls-cert, --tls-key)"
        } else {
            "the server refused the handshake: it may require a client certificate (--tls-cert and --tls-key)"
        }
        .to_owned())
    } else {
        None
    }
}

/// Detail and hint of a failed upgrade.
fn upgrade_failure(
    error: &ConnectError,
    info: &UpgradeInfo,
    expected: &str,
    has_userinfo: bool,
) -> (String, Option<String>) {
    match error {
        ConnectError::HttpStatus { status, line } => (
            format!("the server answered `{}` instead of 101", clean(line, 200)),
            status_hint(*status, has_userinfo),
        ),
        ConnectError::SubprotocolMismatch { .. } => (
            format!(
                "the server selected subprotocol {} instead of `{}`",
                info.protocol
                    .as_deref()
                    .map_or_else(|| "none".to_owned(), |p| format!("`{p}`")),
                clean(expected, 100)
            ),
            Some(
                "set --ws-subprotocol to a value the server accepts; OCPP 1.6J uses ocpp1.6"
                    .to_owned(),
            ),
        ),
        other => (clean(&other.to_string(), 300), None),
    }
}

/// What to try after an HTTP status other than 101.
#[must_use]
pub fn status_hint(status: u16, has_userinfo: bool) -> Option<String> {
    let text = match status {
        401 if has_userinfo => {
            "the Central System rejected the credentials: check the user (the charge point id) and the password (the AuthorizationKey) in the URL"
        }
        401 => {
            "the Central System requires Basic authentication: put user:password@ into the URL (Security Profile 1 or 2; the user is the charge point id)"
        }
        403 => {
            "the Central System refuses this charge point: check that the id is registered and that the credentials match"
        }
        404 => {
            "no endpoint at this path: the charge point id is appended to the URL path as its last segment, so check the path of the URL"
        }
        400 => {
            "the Central System refused the upgrade: check --ws-subprotocol and the charge point id"
        }
        301 | 302 | 303 | 307 | 308 => {
            "the Central System redirects; redirects are not followed, so use the target URL"
        }
        502..=504 => {
            "a proxy in front of the Central System cannot reach it: check the upstream service"
        }
        500..=599 => "the Central System failed internally: check its logs",
        _ => return None,
    };
    Some(text.to_owned())
}

/// The TLS version and the leaf certificate of an established connection.
fn tls_facts<IO>(stream: &tokio_rustls::client::TlsStream<IO>) -> (String, Option<CertFacts>) {
    let connection = stream.get_ref().1;
    let version = match connection.protocol_version() {
        Some(rustls::ProtocolVersion::TLSv1_3) => "TLS 1.3".to_owned(),
        Some(rustls::ProtocolVersion::TLSv1_2) => "TLS 1.2".to_owned(),
        Some(other) => format!("{other:?}"),
        None => "unknown TLS version".to_owned(),
    };
    let cert = connection
        .peer_certificates()
        .and_then(|chain| chain.first())
        .and_then(|der| parse_certificate(der.as_ref()));
    (version, cert)
}

/// Subject, issuer and expiry of a DER certificate.
#[must_use]
pub fn parse_certificate(der: &[u8]) -> Option<CertFacts> {
    use x509_parser::prelude::{FromDer, X509Certificate};
    let (_, cert) = X509Certificate::from_der(der).ok()?;
    Some(CertFacts {
        subject: clean(&cert.subject().to_string(), 200),
        issuer: clean(&cert.issuer().to_string(), 200),
        not_after: DateTime::from_timestamp(cert.validity().not_after.timestamp(), 0)?,
    })
}

/// The certificate of a server whose certificate fails verification, read with verification off.
async fn describe_unverified(
    addr: SocketAddr,
    url: &Url,
    o: &ProbeOptions,
    cap: Duration,
) -> Option<CertFacts> {
    let lenient = TlsConfig {
        skip_verify: true,
        server_name: o.tls.server_name.clone(),
        ..TlsConfig::default()
    };
    let config = tls::client_config(&lenient).await.ok()?;
    let name = tls::server_name(url, &lenient).ok()?;
    timeout(cap, async {
        let tcp = TcpStream::connect(addr).await.ok()?;
        let stream = TlsConnector::from(config).connect(name, tcp).await.ok()?;
        tls_facts(&stream).1
    })
    .await
    .ok()
    .flatten()
}

/// Expiry of a certificate as a status and a phrase. An expired certificate fails the step
/// when it is verified; with verification off the connection works, so it only warns.
#[must_use]
pub fn classify_expiry(
    not_after: DateTime<Utc>,
    now: DateTime<Utc>,
    verifying: bool,
) -> (StepStatus, String) {
    let date = not_after.format("%Y-%m-%d");
    let remaining = not_after - now;
    if remaining < chrono::Duration::zero() {
        let days = (-remaining).num_days();
        let status = if verifying {
            StepStatus::Failed
        } else {
            StepStatus::Warning
        };
        (status, format!("expired on {date} ({days} days ago)"))
    } else if remaining.num_days() < CERT_WARN_DAYS {
        (
            StepStatus::Warning,
            format!("expires on {date} (in {} days)", remaining.num_days()),
        )
    } else {
        (
            StepStatus::Passed,
            format!("valid until {date} ({} days left)", remaining.num_days()),
        )
    }
}

fn cert_summary(cert: &CertFacts, now: DateTime<Utc>) -> String {
    let (_, expiry) = classify_expiry(cert.not_after, now, true);
    format!("subject {}, issuer {}, {expiry}", cert.subject, cert.issuer)
}

/// Status and detail of a completed TLS handshake.
fn describe_tls(
    version: &str,
    cert: Option<&CertFacts>,
    tls: &TlsConfig,
    now: DateTime<Utc>,
) -> (StepStatus, String) {
    let verification = if tls.skip_verify {
        "certificate verification skipped (--tls-skip-verify)".to_owned()
    } else if tls.ca_file.is_some() {
        "certificate verified against --tls-ca".to_owned()
    } else {
        "certificate verified against the system roots".to_owned()
    };
    let client = if tls.cert_file.is_some() {
        ", client certificate sent"
    } else {
        ""
    };
    match cert {
        Some(cert) => {
            let (status, expiry) = classify_expiry(cert.not_after, now, !tls.skip_verify);
            (
                status,
                format!(
                    "{version}; subject {}, issuer {}, {expiry}; {verification}{client}",
                    cert.subject, cert.issuer
                ),
            )
        }
        None => (
            StepStatus::Passed,
            format!("{version}; the certificate could not be read; {verification}{client}"),
        ),
    }
}

/// Status, detail and hint of a `BootNotification` answer.
#[must_use]
pub fn classify_boot(
    status: RegistrationStatus,
    interval: i32,
    skew_secs: i64,
) -> (StepStatus, String, Option<String>) {
    let skew = format!("server time {skew_secs:+} s from this machine");
    match status {
        RegistrationStatus::Accepted => {
            let detail = format!("Accepted, heartbeat interval {interval} s, {skew}");
            if skew_secs.abs() > SKEW_WARN_SECS {
                (
                    StepStatus::Warning,
                    detail,
                    Some("the clocks differ by more than a minute: check the time of this machine or of the Central System".to_owned()),
                )
            } else {
                (StepStatus::Passed, detail, None)
            }
        }
        RegistrationStatus::Pending => (
            StepStatus::Warning,
            format!("Pending, retry interval {interval} s, {skew}"),
            Some("the Central System has not accepted this charge point yet: register the id there".to_owned()),
        ),
        RegistrationStatus::Rejected => (
            StepStatus::Failed,
            format!("Rejected, retry interval {interval} s, {skew}"),
            Some("the Central System refuses this charge point: check that the id (and vendor and model) is known there".to_owned()),
        ),
    }
}

/// Detail and hint of a `BootNotification` that got no usable answer.
fn boot_failure(error: &CallError) -> (String, Option<String>) {
    let detail = clean(&error.to_string(), 300);
    let hint = match error {
        CallError::Timeout { .. } => Some(
            "the Central System accepted the connection but never answered: check its logs, or raise --call-timeout-ms and --timeout"
                .to_owned(),
        ),
        CallError::Closed { .. } => Some(
            "the Central System closed the connection right after the upgrade: it may reject this id or protocol after the handshake"
                .to_owned(),
        ),
        _ => None,
    };
    (detail, hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn expiry_is_classified_by_the_days_left() {
        let now = at("2026-01-01T00:00:00Z");
        let (status, text) = classify_expiry(at("2026-06-01T00:00:00Z"), now, true);
        assert_eq!(status, StepStatus::Passed);
        assert!(text.contains("valid until 2026-06-01"), "{text}");
        let (status, text) = classify_expiry(at("2026-01-10T00:00:00Z"), now, true);
        assert_eq!(status, StepStatus::Warning);
        assert!(text.contains("in 9 days"), "{text}");
        let (status, _) = classify_expiry(at("2026-01-15T00:00:00Z"), now, true);
        assert_eq!(status, StepStatus::Passed, "exactly 14 days is not soon");
    }

    #[test]
    fn an_expired_certificate_fails_only_when_verified() {
        let now = at("2026-01-01T00:00:00Z");
        let expired = at("2025-12-20T00:00:00Z");
        let (status, text) = classify_expiry(expired, now, true);
        assert_eq!(status, StepStatus::Failed);
        assert!(text.contains("12 days ago"), "{text}");
        assert_eq!(classify_expiry(expired, now, false).0, StepStatus::Warning);
    }

    #[test]
    fn boot_statuses_map_to_step_statuses() {
        let (status, detail, hint) = classify_boot(RegistrationStatus::Accepted, 300, 2);
        assert_eq!(status, StepStatus::Passed);
        assert!(
            detail.contains("Accepted, heartbeat interval 300 s"),
            "{detail}"
        );
        assert!(detail.contains("+2 s"), "{detail}");
        assert!(hint.is_none());
        assert_eq!(
            classify_boot(RegistrationStatus::Accepted, 300, -3600).0,
            StepStatus::Warning
        );
        assert_eq!(
            classify_boot(RegistrationStatus::Pending, 30, 0).0,
            StepStatus::Warning
        );
        let (status, _, hint) = classify_boot(RegistrationStatus::Rejected, 30, 0);
        assert_eq!(status, StepStatus::Failed);
        assert!(hint.is_some());
    }

    #[test]
    fn status_hints_cover_the_common_refusals() {
        assert!(status_hint(401, false).unwrap().contains("user:password@"));
        assert!(
            status_hint(401, true)
                .unwrap()
                .contains("rejected the credentials")
        );
        assert!(status_hint(403, false).unwrap().contains("registered"));
        assert!(status_hint(404, false).unwrap().contains("path"));
        assert!(status_hint(503, false).unwrap().contains("proxy"));
        assert!(status_hint(418, false).is_none());
    }

    #[test]
    fn tls_hints_name_the_flag_to_use() {
        assert!(
            tls_hint("invalid peer certificate: UnknownIssuer", false)
                .unwrap()
                .contains("--tls-ca")
        );
        assert!(
            tls_hint("invalid peer certificate: NotValidForName", false)
                .unwrap()
                .contains("--tls-server-name")
        );
        assert!(
            tls_hint("received fatal alert: BadCertificate", false)
                .unwrap()
                .contains("client certificate")
        );
        assert!(tls_hint("something else", false).is_none());
    }

    #[test]
    fn tcp_hints_depend_on_the_error_kind() {
        use std::io::ErrorKind;
        assert!(
            tcp_hint(Some(ErrorKind::ConnectionRefused))
                .unwrap()
                .contains("listens")
        );
        assert!(
            tcp_hint(Some(ErrorKind::TimedOut))
                .unwrap()
                .contains("firewall")
        );
        assert!(tcp_hint(None).is_none());
    }

    #[test]
    fn the_shown_url_hides_the_password_and_keeps_the_user() {
        let url = Url::parse("wss://CP-1:s3cret@host/ocpp/CP-1").unwrap();
        let shown = shown_url(&url);
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(shown.contains("CP-1:***@host"), "{shown}");
    }

    #[test]
    fn the_shown_url_keeps_query_names_and_hides_values() {
        let url = Url::parse("wss://host/ocpp/CP-1?token=s3cret&x=1").unwrap();
        let shown = shown_url(&url);
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(shown.ends_with("?token=***&x=***"), "{shown}");
    }

    #[test]
    fn server_text_is_stripped_of_control_characters() {
        assert_eq!(clean("a\u{1b}[31mb\nc", 100), "a[31mbc");
        assert_eq!(clean("abcdef", 3), "abc");
    }

    #[test]
    fn report_json_has_a_fixed_key_set() {
        let report = Report {
            schema: SCHEMA_VERSION,
            ocpp_version: OCPP_VERSION,
            tool_version: TOOL_VERSION,
            url: "ws://h/ocpp/CP".to_owned(),
            status: "passed",
            steps: vec![Step {
                name: "dns",
                status: StepStatus::Warning,
                detail: "d".to_owned(),
                hint: None,
                duration_ms: 3,
            }],
        };
        let value = serde_json::to_value(&report).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "ocpp_version",
                "schema",
                "status",
                "steps",
                "tool_version",
                "url"
            ]
        );
        let step = value["steps"][0].as_object().unwrap();
        let mut keys: Vec<&str> = step.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["detail", "duration_ms", "hint", "name", "status"]);
        assert_eq!(value["steps"][0]["status"], "warning");
        assert!(value["steps"][0]["hint"].is_null());
    }
}
