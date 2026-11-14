//! Security Whitepaper on the CSMS side: connection security profiles 1 to 3, tracked security
//! reports and the typed commands, with real `kirby-ocpp-core` stations as clients.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use kirby_csms_core::{
    CsmsConfig, CsmsError, CsmsEvent, CsmsHandle, CsmsServer, SecurityProfile, StationState,
    SubprotocolPolicy, server_config_from_pem, server_config_with_client_ca,
};
use kirby_ocpp_core::config::{ReconnectConfig, TlsConfig};
use kirby_ocpp_core::ocpp::messages::{
    CertificateUse, ExtendedMessageTrigger, GetInstalledCertificateStatus, GetLogRequest,
    LogParameters, LogType, SignedFirmware, SignedFirmwareStatus, SignedUpdateFirmwareRequest,
    UploadLogStatus,
};
use kirby_ocpp_core::ocpp::types::RegistrationStatus;
use kirby_ocpp_core::station::Command;
use kirby_ocpp_core::{Station, StationConfig, StationHandle, SystemClock};
use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const STEP: Duration = Duration::from_secs(10);
const KEY: &str = "0123456789abcdef0123";

// ---- PKI -----------------------------------------------------------------------------

struct Ca {
    pem: String,
    issuer: rcgen::Issuer<'static, KeyPair>,
}

fn name(cn: &str) -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, cn);
    dn
}

fn ca(cn: &str) -> Ca {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name = name(cn);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let cert = params.self_signed(&key).unwrap();
    Ca {
        pem: cert.pem(),
        issuer: rcgen::Issuer::new(params, key),
    }
}

/// `(certificate PEM, key PEM)` signed by `ca`.
fn leaf(ca: &Ca, cn: &str, sans: Vec<String>) -> (String, String) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(sans).unwrap();
    params.distinguished_name = name(cn);
    let cert = params.signed_by(&key, &ca.issuer).unwrap();
    (cert.pem(), key.serialize_pem())
}

fn write(dir: &Path, file: &str, text: &str) -> std::path::PathBuf {
    let path = dir.join(file);
    std::fs::write(&path, text).unwrap();
    path
}

// ---- engines and stations --------------------------------------------------------------

async fn start(tweak: impl FnOnce(&mut CsmsConfig)) -> CsmsHandle {
    let mut config = CsmsConfig::new("127.0.0.1:0".parse().unwrap());
    tweak(&mut config);
    CsmsServer::start(config).await.expect("engine starts")
}

fn basic_auth(config: &mut CsmsConfig, profile: SecurityProfile) {
    config.security.profile = profile;
    config
        .security
        .keys
        .insert("CP-A".to_owned(), KEY.to_owned());
}

/// A server with TLS from `server`'s certificate; `client_ca` requires client certificates.
fn tls_config(server: &Ca, client_ca: Option<&Ca>) -> Arc<rustls::ServerConfig> {
    let (cert, key) = leaf(server, "csms", vec!["127.0.0.1".to_owned()]);
    let chain = format!("{cert}\n{}", server.pem);
    match client_ca {
        Some(client_ca) => {
            server_config_with_client_ca(chain.as_bytes(), key.as_bytes(), client_ca.pem.as_bytes())
        }
        None => server_config_from_pem(chain.as_bytes(), key.as_bytes()),
    }
    .unwrap()
}

struct Cp {
    handle: StationHandle,
    _dir: TempDir,
}

impl Cp {
    /// Spawns a station for the engine; `key` sets the AuthorizationKey and `profile` the
    /// SecurityProfile configuration (profile 1 or 2) before connecting. Does not connect.
    async fn spawn(
        csms: &CsmsHandle,
        id: &str,
        tls: TlsConfig,
        key: Option<&str>,
        profile: Option<&str>,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut config = StationConfig::new(format!("{}/ocpp", csms.url()))
            .with_charge_point_id(id)
            .with_call_timeout_ms(3_000)
            .with_tls(tls)
            .with_reconnect(ReconnectConfig {
                enabled: false,
                initial_delay_ms: 30,
                max_delay_ms: 100,
                max_attempts: 0,
            })
            .with_state_dir(dir.path());
        config.ws_ping_interval_secs = 0;
        let (handle, _join) = Station::spawn(config, Arc::new(SystemClock));
        for (config_key, value) in [("AuthorizationKey", key), ("SecurityProfile", profile)] {
            if let Some(value) = value {
                handle
                    .send(Command::SetConfiguration {
                        key: config_key.to_owned(),
                        value: value.to_owned(),
                    })
                    .await
                    .unwrap_or_else(|e| panic!("{config_key}: {e}"));
            }
        }
        Self { handle, _dir: dir }
    }

    async fn connected(csms: &CsmsHandle, id: &str) -> Self {
        let cp = Self::spawn(csms, id, TlsConfig::default(), None, None).await;
        cp.handle.connect().await.unwrap();
        cp.wait_accepted().await;
        cp
    }

    async fn wait_accepted(&self) {
        let mut rx = self.handle.watch_snapshot();
        timeout(
            STEP,
            rx.wait_for(|s| s.registration == Some(RegistrationStatus::Accepted)),
        )
        .await
        .expect("boot accepted in time")
        .expect("station alive");
    }
}

/// Waits for an event; ignores everything else.
async fn wait_event<T>(
    rx: &mut broadcast::Receiver<CsmsEvent>,
    pick: impl Fn(&CsmsEvent) -> Option<T>,
) -> T {
    timeout(STEP, async {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Some(found) = pick(&event) {
                        return found;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("events closed"),
            }
        }
    })
    .await
    .expect("event in time")
}

async fn wait_connected(rx: &mut broadcast::Receiver<CsmsEvent>, id: &str) {
    wait_event(rx, |e| {
        matches!(e, CsmsEvent::Connected { station } if station == id).then_some(())
    })
    .await;
}

async fn wait_refused(
    rx: &mut broadcast::Receiver<CsmsEvent>,
    id: Option<&str>,
) -> (Option<String>, String) {
    wait_event(rx, |e| match e {
        CsmsEvent::AuthFailed {
            station, reason, ..
        } if station.as_deref() == id => Some((station.clone(), reason.clone())),
        _ => None,
    })
    .await
}

async fn wait_state(
    csms: &CsmsHandle,
    rx: &mut broadcast::Receiver<CsmsEvent>,
    id: &str,
    done: impl Fn(&StationState) -> bool,
) -> StationState {
    timeout(STEP, async {
        loop {
            if let Some(state) = csms.state(id)
                && done(&state)
            {
                return state;
            }
            match rx.recv().await {
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("events closed"),
            }
        }
    })
    .await
    .expect("state in time")
}

/// A raw WebSocket handshake with an optional `Authorization` header; the HTTP refusal if any.
async fn raw_handshake(
    csms: &CsmsHandle,
    id: &str,
    authorization: Option<&str>,
) -> Result<(), (u16, Option<String>)> {
    raw_handshake_offering(csms, id, authorization, Some("ocpp1.6")).await
}

/// Like [`raw_handshake`], offering `protocol` (`None` offers nothing).
async fn raw_handshake_offering(
    csms: &CsmsHandle,
    id: &str,
    authorization: Option<&str>,
    protocol: Option<&str>,
) -> Result<(), (u16, Option<String>)> {
    let mut request = format!("{}/ocpp/{id}", csms.url())
        .into_client_request()
        .unwrap();
    let headers = request.headers_mut();
    if let Some(protocol) = protocol {
        headers.insert("Sec-WebSocket-Protocol", protocol.parse().unwrap());
    }
    if let Some(value) = authorization {
        headers.insert("Authorization", value.parse().unwrap());
    }
    match tokio_tungstenite::connect_async(request).await {
        Ok(_) => Ok(()),
        Err(WsError::Http(response)) => Err((
            response.status().as_u16(),
            response
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
        )),
        Err(other) => panic!("unexpected error: {other}"),
    }
}

fn basic(user: &str, password: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )
}

/// A Basic header for a user and raw password bytes.
fn basic_bytes(user: &str, password: &[u8]) -> String {
    let mut raw = format!("{user}:").into_bytes();
    raw.extend_from_slice(password);
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    )
}

// ---- profile 1 and 2 ---------------------------------------------------------------------

#[tokio::test]
async fn misconfigured_profiles_do_not_start() {
    for profile in [SecurityProfile::BasicAuthTls, SecurityProfile::MutualTls] {
        let mut config = CsmsConfig::new("127.0.0.1:0".parse().unwrap());
        config.security.profile = profile;
        let error = CsmsServer::start(config).await.unwrap_err();
        assert!(matches!(error, CsmsError::Config(_)), "{error}");
    }
    // Profile 1 and 0 need no TLS.
    start(|c| c.security.profile = SecurityProfile::BasicAuth)
        .await
        .shutdown()
        .await;
}

#[tokio::test]
async fn empty_keys_do_not_start() {
    for tweak in [
        (|c: &mut CsmsConfig| c.security.default_key = Some(String::new())) as fn(&mut _),
        |c| {
            c.security.keys.insert("CP-A".to_owned(), String::new());
        },
    ] {
        let mut config = CsmsConfig::new("127.0.0.1:0".parse().unwrap());
        config.security.profile = SecurityProfile::BasicAuth;
        tweak(&mut config);
        let error = CsmsServer::start(config).await.unwrap_err();
        assert!(matches!(error, CsmsError::Config(_)), "{error}");
    }
}

#[tokio::test]
async fn oversized_station_ids_are_refused_before_anything_is_recorded() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let long = "a".repeat(129);
    assert_eq!(raw_handshake(&csms, &long, None).await.unwrap_err().0, 400);
    assert_eq!(
        raw_handshake(&csms, "CP%0A1", None).await.unwrap_err().0,
        400
    );
    assert_eq!(csms.stations().len(), 0);
    // Each refusal is reported (without the unusable id); nothing else is emitted.
    for _ in 0..2 {
        match events.try_recv() {
            Ok(CsmsEvent::AuthFailed {
                station: None,
                reason,
                ..
            }) => assert!(reason.contains("station id"), "{reason}"),
            other => panic!("expected an AuthFailed event, got {other:?}"),
        }
    }
    assert!(events.try_recv().is_err());
    assert!(raw_handshake(&csms, "CP-OK", None).await.is_ok());
    csms.shutdown().await;
}

#[tokio::test]
async fn profile_one_accepts_the_right_key_and_refuses_the_rest() {
    let csms = start(|c| basic_auth(c, SecurityProfile::BasicAuth)).await;
    let mut events = csms.subscribe();

    let good = Cp::spawn(&csms, "CP-A", TlsConfig::default(), Some(KEY), Some("1")).await;
    good.handle.connect().await.unwrap();
    good.wait_accepted().await;
    wait_connected(&mut events, "CP-A").await;

    // The real station with a wrong key (same id, after the first one is gone).
    let wrong = Cp::spawn(
        &csms,
        "CP-A",
        TlsConfig::default(),
        Some("ffffffffffffffffffff"),
        Some("1"),
    )
    .await;
    wrong.handle.connect().await.ok();
    let (station, reason) = wait_refused(&mut events, Some("CP-A")).await;
    assert_eq!(station.as_deref(), Some("CP-A"));
    assert_eq!(reason, "invalid credentials");
    // The refused station did not replace the good one.
    assert!(csms.station("CP-A").is_some_and(|s| s.connected));

    // No header, wrong user, wrong password, no key for the id, garbage header.
    let cases = [
        (None, "CP-A", "missing or malformed credentials"),
        (
            Some("Bearer abc".to_owned()),
            "CP-A",
            "missing or malformed credentials",
        ),
        (
            Some("Basic !!!".to_owned()),
            "CP-A",
            "missing or malformed credentials",
        ),
        (Some(basic("CP-X", KEY)), "CP-A", "invalid credentials"),
        (Some(basic("CP-A", "nope")), "CP-A", "invalid credentials"),
        (
            Some(basic("CP-A", &format!("{KEY}x"))),
            "CP-A",
            "invalid credentials",
        ),
        (
            Some(basic("CP-N", KEY)),
            "CP-N",
            "no key configured for the station",
        ),
    ];
    for (header, id, expected) in cases {
        let (status, challenge) = raw_handshake(&csms, id, header.as_deref())
            .await
            .unwrap_err();
        assert_eq!(status, 401, "{expected}");
        assert!(challenge.is_some_and(|c| c.starts_with("Basic realm=")));
        let (_, reason) = wait_refused(&mut events, Some(id)).await;
        assert_eq!(reason, expected);
    }
    // The right credentials pass.
    assert!(
        raw_handshake(&csms, "CP-A", Some(&basic("CP-A", KEY)))
            .await
            .is_ok()
    );
    assert!(csms.station("CP-N").is_none());
    csms.shutdown().await;
}

/// Specification example: 20 bytes ending in a run of `0xFF`, which no UTF-8 text can hold.
const SPEC_BYTES: [u8; 20] = [
    0, 1, 2, 3, 4, 5, 6, 7, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
];
const SPEC_HEX_UPPER: &str = "0001020304050607FFFFFFFFFFFFFFFFFFFFFFFF";

/// The decoded form of a hex key is accepted next to the literal one for OCPP 1.6 only.
#[tokio::test]
async fn profile_one_accepts_decoded_hex_keys_on_ocpp_16() {
    let csms = start(|c| {
        c.security.profile = SecurityProfile::BasicAuth;
        for (id, key) in [
            ("AL1000", SPEC_HEX_UPPER.to_owned()),
            ("AL1001", SPEC_HEX_UPPER.to_lowercase()),
            (
                "AL1002",
                "0001020304050607fFfFfFfFfFfFfFfFfFfFfFfF".to_owned(),
            ),
            ("CP-COLON", "00013a0405".to_owned()),
            ("CP-TEXT", "6162".to_owned()),
            ("CP-SHORT", "ab".to_owned()),
            ("CP-ODD", "abc".to_owned()),
            ("CP-NONHEX", "zz11".to_owned()),
        ] {
            c.security.keys.insert(id.to_owned(), key);
        }
    })
    .await;
    let mut events = csms.subscribe();
    let mut wrong_last = SPEC_BYTES;
    wrong_last[19] = 254;
    let mut extra = SPEC_BYTES.to_vec();
    extra.push(0);

    let accepted: [(&str, Vec<u8>); 13] = [
        ("AL1000", SPEC_BYTES.to_vec()),
        ("AL1000", SPEC_HEX_UPPER.as_bytes().to_vec()),
        ("AL1001", SPEC_BYTES.to_vec()),
        ("AL1001", SPEC_HEX_UPPER.to_lowercase().into_bytes()),
        ("AL1002", SPEC_BYTES.to_vec()),
        ("CP-COLON", vec![0, 1, b':', 4, 5]),
        ("CP-TEXT", b"ab".to_vec()),
        ("CP-TEXT", b"6162".to_vec()),
        ("CP-SHORT", vec![0xab]),
        ("CP-SHORT", b"ab".to_vec()),
        ("CP-ODD", b"abc".to_vec()),
        ("CP-NONHEX", b"zz11".to_vec()),
        // The literal key is matched byte for byte, in its own case.
        (
            "AL1002",
            b"0001020304050607fFfFfFfFfFfFfFfFfFfFfFfF".to_vec(),
        ),
    ];
    for (id, password) in accepted {
        let header = basic_bytes(id, &password);
        assert!(
            raw_handshake(&csms, id, Some(&header)).await.is_ok(),
            "{id} {password:02x?}"
        );
    }

    let refused: [(&str, Vec<u8>); 10] = [
        ("AL1000", wrong_last.to_vec()),
        ("AL1000", extra),
        ("AL1000", SPEC_BYTES[..19].to_vec()),
        ("AL1000", Vec::new()),
        // Only the literal key may differ in case, and only as written.
        ("AL1001", SPEC_HEX_UPPER.as_bytes().to_vec()),
        ("AL1002", SPEC_HEX_UPPER.as_bytes().to_vec()),
        // An odd-length or non-hex key has no decoded form.
        ("CP-ODD", vec![0xab, 0x0c]),
        ("CP-NONHEX", vec![b'z', b'z', 0x11]),
        ("CP-COLON", vec![0, 1]),
        ("CP-SHORT", vec![0xab, 0xab]),
    ];
    for (id, password) in refused {
        let header = basic_bytes(id, &password);
        let (status, challenge) = raw_handshake(&csms, id, Some(&header)).await.unwrap_err();
        assert_eq!(status, 401, "{id} {password:02x?}");
        // A binary key is allowed here, so the challenge does not promise UTF-8.
        assert_eq!(challenge.as_deref(), Some("Basic realm=\"kirby-csms\""));
        let (_, reason) = wait_refused(&mut events, Some(id)).await;
        assert_eq!(reason, "invalid credentials");
    }

    csms.shutdown().await;
}

/// User, malformed-header and missing-key refusals next to a binary key.
#[tokio::test]
async fn binary_keys_keep_the_other_refusal_reasons() {
    let csms = start(|c| {
        c.security.profile = SecurityProfile::BasicAuth;
        c.security
            .keys
            .insert("AL1000".to_owned(), SPEC_HEX_UPPER.to_owned());
    })
    .await;
    let mut events = csms.subscribe();
    // The user is compared as bytes: a right password with a wrong or non-UTF-8 user fails.
    let mut bad_user = b"AL1000".to_vec();
    bad_user.push(0xff);
    bad_user.push(b':');
    bad_user.extend_from_slice(&SPEC_BYTES);
    for raw in [[b"CP-X:".as_slice(), &SPEC_BYTES].concat(), bad_user] {
        let header = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(raw)
        );
        assert_eq!(
            raw_handshake(&csms, "AL1000", Some(&header))
                .await
                .unwrap_err()
                .0,
            401
        );
        let (_, reason) = wait_refused(&mut events, Some("AL1000")).await;
        assert_eq!(reason, "invalid credentials");
    }

    // Malformed headers keep their reason, whatever the key looks like.
    let nocolon = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(b"AL1000")
    );
    for header in ["Basic !!!", "Bearer abc", nocolon.as_str()] {
        assert_eq!(
            raw_handshake(&csms, "AL1000", Some(header))
                .await
                .unwrap_err()
                .0,
            401
        );
        let (_, reason) = wait_refused(&mut events, Some("AL1000")).await;
        assert_eq!(reason, "missing or malformed credentials");
    }
    // A station without a key keeps its own reason.
    let header = basic_bytes("CP-N", &SPEC_BYTES);
    assert_eq!(
        raw_handshake(&csms, "CP-N", Some(&header))
            .await
            .unwrap_err()
            .0,
        401
    );
    let (_, reason) = wait_refused(&mut events, Some("CP-N")).await;
    assert_eq!(reason, "no key configured for the station");
    csms.shutdown().await;
}

#[tokio::test]
async fn decoded_hex_keys_are_limited_to_ocpp_16_sessions() {
    let csms = start(|c| {
        c.security.profile = SecurityProfile::BasicAuth;
        c.security
            .keys
            .insert("AL1000".to_owned(), SPEC_HEX_UPPER.to_owned());
        c.subprotocol = SubprotocolPolicy::RequireAny(vec![
            "ocpp1.6".to_owned(),
            "ocpp2.0.1".to_owned(),
            "ocpp2.1".to_owned(),
        ]);
    })
    .await;
    let decoded = basic_bytes("AL1000", &SPEC_BYTES);
    let literal = basic_bytes("AL1000", SPEC_HEX_UPPER.as_bytes());
    for (offer, decoded_ok) in [
        ("ocpp1.6", true),
        ("ocpp2.0.1", false),
        ("ocpp2.1", false),
        // The client's first listed supported name is selected.
        ("ocpp2.0.1, ocpp1.6", false),
        ("ocpp1.6, ocpp2.0.1", true),
    ] {
        let result = raw_handshake_offering(&csms, "AL1000", Some(&decoded), Some(offer)).await;
        if decoded_ok {
            assert!(result.is_ok(), "{offer}");
        } else {
            let (status, challenge) = result.unwrap_err();
            assert_eq!(status, 401, "{offer}");
            // UTF-8 is the only form there, so the challenge says so.
            assert_eq!(
                challenge.as_deref(),
                Some("Basic realm=\"kirby-csms\", charset=\"UTF-8\""),
                "{offer}"
            );
        }
        // The literal key works on every protocol.
        assert!(
            raw_handshake_offering(&csms, "AL1000", Some(&literal), Some(offer))
                .await
                .is_ok(),
            "{offer}"
        );
    }
    csms.shutdown().await;
}

/// A 40-digit hex key is a plain UTF-8 password on OCPP 2.0.1: only the literal text
/// authenticates, the decoded bytes never do. OCPP 1.6 accepts both.
#[tokio::test]
async fn profile_one_on_ocpp_201_takes_the_literal_key_only() {
    let csms = start(|c| {
        c.security.profile = SecurityProfile::BasicAuth;
        c.security
            .keys
            .insert("AL1000".to_owned(), SPEC_HEX_UPPER.to_owned());
        c.subprotocol =
            SubprotocolPolicy::RequireAny(vec!["ocpp1.6".to_owned(), "ocpp2.0.1".to_owned()]);
    })
    .await;
    let mut events = csms.subscribe();
    let decoded = basic_bytes("AL1000", &SPEC_BYTES);
    let literal = basic("AL1000", SPEC_HEX_UPPER);

    // 2.0.1: the literal key passes, the decoded one is a 401 with the UTF-8 hint.
    let accepted = raw_handshake_offering(&csms, "AL1000", Some(&literal), Some("ocpp2.0.1")).await;
    assert!(accepted.is_ok());
    let (status, challenge) =
        raw_handshake_offering(&csms, "AL1000", Some(&decoded), Some("ocpp2.0.1"))
            .await
            .unwrap_err();
    assert_eq!(status, 401);
    assert_eq!(
        challenge.as_deref(),
        Some("Basic realm=\"kirby-csms\", charset=\"UTF-8\"")
    );
    let (_, reason) = wait_refused(&mut events, Some("AL1000")).await;
    assert_eq!(reason, "invalid credentials");

    // 1.6: both forms pass.
    for header in [&literal, &decoded] {
        let accepted = raw_handshake_offering(&csms, "AL1000", Some(header), Some("ocpp1.6")).await;
        assert!(accepted.is_ok());
    }
    csms.shutdown().await;
}

/// On OCPP 2.0.1 the id rule is judged before the credentials: a bad id is a 400 whatever the
/// credentials are, and bad credentials for a valid id stay a 401.
#[tokio::test]
async fn the_ocpp_201_station_id_rule_comes_before_the_credentials() {
    let csms = start(|c| {
        basic_auth(c, SecurityProfile::BasicAuth);
        c.subprotocol =
            SubprotocolPolicy::RequireAny(vec!["ocpp1.6".to_owned(), "ocpp2.0.1".to_owned()]);
    })
    .await;
    let mut events = csms.subscribe();
    let id = "CP:A";
    for header in [None, Some(basic(id, "nope")), Some(basic(id, KEY))] {
        let result = raw_handshake_offering(&csms, id, header.as_deref(), Some("ocpp2.0.1")).await;
        assert_eq!(result.unwrap_err().0, 400, "{header:?}");
        let (station, reason) = wait_refused(&mut events, None).await;
        assert_eq!(station, None);
        assert!(
            reason.starts_with("refused: station id is not valid"),
            "{reason}"
        );
    }
    // A valid id with wrong credentials is a 401.
    let wrong = basic("CP-A", "nope");
    let result = raw_handshake_offering(&csms, "CP-A", Some(&wrong), Some("ocpp2.0.1")).await;
    assert_eq!(result.unwrap_err().0, 401);
    // The same colon id is not refused for its form on 1.6 (it fails the key check only).
    let result = raw_handshake_offering(&csms, id, Some(&basic(id, KEY)), Some("ocpp1.6")).await;
    assert_eq!(
        result.unwrap_err().0,
        401,
        "no key is configured for that id"
    );
    csms.shutdown().await;
}

#[tokio::test]
async fn authentication_is_judged_before_the_protocol_refusal() {
    let csms = start(|c| {
        basic_auth(c, SecurityProfile::BasicAuth);
        c.security
            .keys
            .insert("AL1000".to_owned(), SPEC_HEX_UPPER.to_owned());
    })
    .await;
    // Only 1.6 is served and the client offers only 2.0.1.
    let offer = Some("ocpp2.0.1");
    let cases = [
        // Wrong credentials stay a 401 whatever the protocol.
        (basic("AL1000", "nope"), 401),
        // Right literal credentials pass, then the protocol is refused.
        (basic("AL1000", SPEC_HEX_UPPER), 400),
        // A failed selection never opens the decoded form.
        (basic_bytes("AL1000", &SPEC_BYTES), 401),
    ];
    for (header, status) in cases {
        let result = raw_handshake_offering(&csms, "AL1000", Some(&header), offer).await;
        assert_eq!(result.unwrap_err().0, status, "{header}");
    }
    csms.shutdown().await;
}

#[tokio::test]
async fn a_session_without_a_selected_subprotocol_is_treated_as_ocpp_16() {
    let csms = start(|c| {
        c.security.profile = SecurityProfile::BasicAuth;
        c.security
            .keys
            .insert("AL1000".to_owned(), SPEC_HEX_UPPER.to_owned());
        c.subprotocol = SubprotocolPolicy::Omit;
    })
    .await;
    let decoded = basic_bytes("AL1000", &SPEC_BYTES);
    assert!(
        raw_handshake_offering(&csms, "AL1000", Some(&decoded), None)
            .await
            .is_ok()
    );
    csms.shutdown().await;
}

#[tokio::test]
async fn default_key_covers_stations_without_an_entry() {
    let csms = start(|c| {
        c.security.profile = SecurityProfile::BasicAuth;
        c.security.default_key = Some(KEY.to_owned());
    })
    .await;
    assert!(
        raw_handshake(&csms, "CP-ANY", Some(&basic("CP-ANY", KEY)))
            .await
            .is_ok()
    );
    assert!(
        raw_handshake(&csms, "CP-ANY", Some(&basic("CP-ANY", "x")))
            .await
            .is_err()
    );
    csms.shutdown().await;
}

#[tokio::test]
async fn profile_two_needs_tls_and_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let server_ca = ca("server ca");
    let tls = tls_config(&server_ca, None);
    let csms = start(|c| {
        basic_auth(c, SecurityProfile::BasicAuthTls);
        c.tls = Some(tls);
    })
    .await;
    assert!(csms.url().starts_with("wss://"));
    let mut events = csms.subscribe();
    let trust = TlsConfig {
        ca_file: Some(write(dir.path(), "ca.pem", &server_ca.pem)),
        ..TlsConfig::default()
    };

    let good = Cp::spawn(&csms, "CP-A", trust.clone(), Some(KEY), Some("2")).await;
    good.handle.connect().await.unwrap();
    good.wait_accepted().await;
    wait_connected(&mut events, "CP-A").await;

    let wrong = Cp::spawn(
        &csms,
        "CP-A",
        trust,
        Some("ffffffffffffffffffff"),
        Some("2"),
    )
    .await;
    wrong.handle.connect().await.ok();
    let (_, reason) = wait_refused(&mut events, Some("CP-A")).await;
    assert_eq!(reason, "invalid credentials");
    csms.shutdown().await;
}

// ---- profile 3 -----------------------------------------------------------------------------

struct Pki {
    server_ca: Ca,
    client_ca: Ca,
    dir: TempDir,
}

impl Pki {
    fn new() -> Self {
        Self {
            server_ca: ca("server ca"),
            client_ca: ca("client ca"),
            dir: tempfile::tempdir().unwrap(),
        }
    }

    async fn csms(&self) -> CsmsHandle {
        let tls = tls_config(&self.server_ca, Some(&self.client_ca));
        start(|c| {
            c.security.profile = SecurityProfile::MutualTls;
            c.tls = Some(tls);
        })
        .await
    }

    /// Station TLS settings with a client certificate with Common Name `cn` signed by `issuer`.
    fn client(&self, issuer: &Ca, cn: &str) -> TlsConfig {
        let (cert, key) = leaf(issuer, cn, Vec::new());
        let stem = format!("{cn}-{}", issuer.pem.len());
        TlsConfig {
            ca_file: Some(write(self.dir.path(), "ca.pem", &self.server_ca.pem)),
            cert_file: Some(write(self.dir.path(), &format!("{stem}.crt"), &cert)),
            key_file: Some(write(self.dir.path(), &format!("{stem}.key"), &key)),
            ..TlsConfig::default()
        }
    }
}

#[tokio::test]
async fn profile_three_accepts_a_matching_certificate() {
    let pki = Pki::new();
    let csms = pki.csms().await;
    let mut events = csms.subscribe();
    let tls = pki.client(&pki.client_ca, "CP-M");
    let cp = Cp::spawn(&csms, "CP-M", tls, None, None).await;
    cp.handle.connect().await.unwrap();
    cp.wait_accepted().await;
    wait_connected(&mut events, "CP-M").await;
    csms.shutdown().await;
}

#[tokio::test]
async fn profile_three_refuses_a_foreign_common_name() {
    let pki = Pki::new();
    let csms = pki.csms().await;
    let mut events = csms.subscribe();
    let tls = pki.client(&pki.client_ca, "CP-OTHER");
    let cp = Cp::spawn(&csms, "CP-M", tls, None, None).await;
    cp.handle.connect().await.ok();
    let (_, reason) = wait_refused(&mut events, Some("CP-M")).await;
    assert_eq!(reason, "client certificate does not match the station id");
    assert!(csms.station("CP-M").is_none());
    csms.shutdown().await;
}

#[tokio::test]
async fn profile_three_refuses_missing_and_untrusted_certificates() {
    let pki = Pki::new();
    let csms = pki.csms().await;
    let mut events = csms.subscribe();

    // No client certificate: the TLS handshake fails.
    let tls = TlsConfig {
        ca_file: Some(write(pki.dir.path(), "ca.pem", &pki.server_ca.pem)),
        ..TlsConfig::default()
    };
    let cp = Cp::spawn(&csms, "CP-M", tls, None, None).await;
    cp.handle.connect().await.ok();
    let (station, reason) = wait_refused(&mut events, None).await;
    assert_eq!((station, reason.as_str()), (None, "TLS handshake failed"));

    // A certificate from some other CA with the right name.
    let rogue = ca("rogue ca");
    let tls = pki.client(&rogue, "CP-M");
    let cp = Cp::spawn(&csms, "CP-M", tls, None, None).await;
    cp.handle.connect().await.ok();
    let (_, reason) = wait_refused(&mut events, None).await;
    assert_eq!(reason, "TLS handshake failed");
    assert!(csms.station("CP-M").is_none());
    csms.shutdown().await;
}

// ---- tracked reports and typed commands -------------------------------------------------------

#[tokio::test]
async fn security_reports_land_in_the_station_state() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let cp = Cp::connected(&csms, "CP-S").await;

    // The station reports a startup event by itself after the first accepted boot.
    let state = wait_state(&csms, &mut events, "CP-S", |s| s.security_events_total >= 1).await;
    assert!(!state.security_events.is_empty());

    cp.handle
        .send(Command::SendSecurityEvent {
            event_type: "ResetOrReboot".to_owned(),
            tech_info: Some("test".to_owned()),
        })
        .await
        .unwrap();
    let state = wait_state(&csms, &mut events, "CP-S", |s| {
        s.security_events
            .iter()
            .any(|e| e.event_type == "ResetOrReboot" && e.tech_info.as_deref() == Some("test"))
    })
    .await;
    assert_eq!(
        csms.station("CP-S")
            .unwrap()
            .received_by_action
            .get("SecurityEventNotification")
            .copied(),
        Some(state.security_events_total)
    );

    cp.handle.send(Command::RequestCertificate).await.unwrap();
    let state = wait_state(&csms, &mut events, "CP-S", |s| {
        s.certificate_request.is_some()
    })
    .await;
    assert!(
        state
            .certificate_request
            .is_some_and(|r| r.csr.contains("CERTIFICATE REQUEST"))
    );

    // GetLog: the station reports Uploading and Uploaded for our request id.
    let reply = csms
        .get_log(
            "CP-S",
            GetLogRequest {
                log_type: LogType::SecurityLog,
                request_id: 41,
                retries: None,
                retry_interval: None,
                log: LogParameters {
                    remote_location: "sftp://u@host/logs".to_owned(),
                    oldest_timestamp: None,
                    latest_timestamp: None,
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(format!("{:?}", reply.status), "Accepted");
    let state = wait_state(&csms, &mut events, "CP-S", |s| {
        s.log_status
            .as_ref()
            .is_some_and(|l| l.status == UploadLogStatus::Uploaded && l.request_id == Some(41))
    })
    .await;
    assert!(state.log_status.is_some());

    // SignedUpdateFirmware: ends with Installed.
    let signer = ca("firmware signer");
    let reply = csms
        .signed_update_firmware(
            "CP-S",
            SignedUpdateFirmwareRequest {
                retries: None,
                retry_interval: None,
                request_id: 42,
                firmware: SignedFirmware {
                    location: "https://host/fw.bin".to_owned(),
                    retrieve_date_time: chrono::DateTime::UNIX_EPOCH,
                    install_date_time: None,
                    signing_certificate: signer.pem.clone(),
                    signature: "c2ln".to_owned(),
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(format!("{:?}", reply.status), "Accepted");
    let state = wait_state(&csms, &mut events, "CP-S", |s| {
        s.signed_firmware_status.as_ref().is_some_and(|f| {
            f.status == SignedFirmwareStatus::Installed && f.request_id == Some(42)
        })
    })
    .await;
    assert!(state.signed_firmware_status.is_some());
    csms.shutdown().await;
}

#[tokio::test]
async fn certificate_commands_round_trip_against_a_station() {
    let csms = start(|_| {}).await;
    let _cp = Cp::connected(&csms, "CP-C").await;
    let root = ca("installed root");
    let kind = CertificateUse::CentralSystemRootCertificate;

    let missing = csms
        .get_installed_certificate_ids("CP-C", kind)
        .await
        .unwrap();
    assert_eq!(missing.status, GetInstalledCertificateStatus::NotFound);

    let installed = csms
        .install_certificate("CP-C", kind, &root.pem)
        .await
        .unwrap();
    assert_eq!(format!("{:?}", installed.status), "Accepted");

    let ids = csms
        .get_installed_certificate_ids("CP-C", kind)
        .await
        .unwrap();
    assert_eq!(ids.status, GetInstalledCertificateStatus::Accepted);
    let hashes = ids.certificate_hash_data.unwrap();
    assert_eq!(hashes.len(), 1);

    let deleted = csms
        .delete_certificate("CP-C", hashes[0].clone())
        .await
        .unwrap();
    assert_eq!(format!("{:?}", deleted.status), "Accepted");
    let again = csms
        .delete_certificate("CP-C", hashes[0].clone())
        .await
        .unwrap();
    assert_eq!(format!("{:?}", again.status), "NotFound");
    let ids = csms
        .get_installed_certificate_ids("CP-C", kind)
        .await
        .unwrap();
    assert_eq!(ids.status, GetInstalledCertificateStatus::NotFound);

    // A chain the station has no pending request for is rejected.
    let rejected = csms
        .certificate_signed("CP-C", root.pem.clone())
        .await
        .unwrap();
    assert_eq!(format!("{:?}", rejected.status), "Rejected");
    csms.shutdown().await;
}

#[tokio::test]
async fn extended_trigger_message_makes_the_station_ask_for_a_certificate() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let _cp = Cp::connected(&csms, "CP-T").await;
    let reply = csms
        .extended_trigger_message(
            "CP-T",
            ExtendedMessageTrigger::SignChargePointCertificate,
            None,
        )
        .await
        .unwrap();
    assert_eq!(format!("{:?}", reply.status), "Accepted");
    wait_state(&csms, &mut events, "CP-T", |s| {
        s.certificate_request.is_some()
    })
    .await;
    csms.shutdown().await;
}

// ---- profile 3 end to end through the station --------------------------------------------

/// The station gets its client certificate through the real `SignCertificate` /
/// `CertificateSigned` flow on a TLS server without client authentication, switches to
/// `SecurityProfile` 3 and is then accepted by a profile 3 server (same address) that only
/// admits certificates of the client CA whose Common Name is the station id.
#[tokio::test]
async fn station_obtains_its_certificate_and_connects_with_profile_three() {
    let pki = Pki::new();
    let id = "CP-E2E";

    // Step 1: server-authenticated TLS (profile 0), no client certificate needed.
    let first = start(|c| c.tls = Some(tls_config(&pki.server_ca, None))).await;
    let addr = first.local_addr();
    let mut events = first.subscribe();
    let tls = TlsConfig {
        ca_file: Some(write(pki.dir.path(), "ca.pem", &pki.server_ca.pem)),
        ..TlsConfig::default()
    };
    let cp = Cp::spawn(&first, id, tls, None, None).await;
    cp.handle.connect().await.unwrap();
    cp.wait_accepted().await;
    // Not every CSMS reply is scripted by default: the engine answers SignCertificate itself.
    cp.handle.send(Command::RequestCertificate).await.unwrap();
    let csr = wait_state(&first, &mut events, id, |s| s.certificate_request.is_some())
        .await
        .certificate_request
        .unwrap()
        .csr;

    // Step 2: the "CA" signs the CSR; the leaf keeps the CSR's subject (CN = station id).
    let leaf = rcgen::CertificateSigningRequestParams::from_pem(&csr)
        .unwrap()
        .signed_by(&pki.client_ca.issuer)
        .unwrap();
    let chain = format!("{}\n{}", leaf.pem(), pki.client_ca.pem);
    let signed = first.certificate_signed(id, chain).await.unwrap();
    assert_eq!(format!("{:?}", signed.status), "Accepted");

    // Step 3: raise the profile (needs wss:// and the stored certificate) and move to a
    // profile 3 server at the same address.
    cp.handle
        .send(Command::SetConfiguration {
            key: "SecurityProfile".to_owned(),
            value: "3".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(cp.handle.snapshot().security_profile, 3);
    first.shutdown().await;
    // Wait until the old server has dropped the session and the station noticed the loss, or
    // `connect` could race with the old socket.
    wait_event(&mut events, |e| {
        matches!(e, CsmsEvent::Disconnected { station, .. } if station == id).then_some(())
    })
    .await;
    let mut link = cp.handle.watch_snapshot();
    timeout(
        STEP,
        link.wait_for(|s| s.connection == kirby_ocpp_core::ws::ConnectionState::Disconnected),
    )
    .await
    .expect("station sees the disconnect")
    .expect("station alive");
    let mut tries = 0;
    let second = loop {
        let tls = tls_config(&pki.server_ca, Some(&pki.client_ca));
        let mut config = CsmsConfig::new(addr);
        config.security.profile = SecurityProfile::MutualTls;
        config.tls = Some(tls);
        match CsmsServer::start(config).await {
            Ok(handle) => break handle,
            Err(_) if tries < 50 => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => panic!("cannot rebind {addr}: {error}"),
        }
    };
    let mut events = second.subscribe();
    cp.handle.connect().await.unwrap();
    cp.wait_accepted().await;
    wait_connected(&mut events, id).await;
    second.shutdown().await;
}
