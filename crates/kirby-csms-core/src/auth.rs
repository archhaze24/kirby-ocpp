//! Connection security checks of the handshake (Security Whitepaper profiles 1 to 3).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request};
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue, StatusCode};

use crate::config::{SecurityPolicy, SecurityProfile};

const AUTHORIZATION: HeaderName = HeaderName::from_static("authorization");
const WWW_AUTHENTICATE: HeaderName = HeaderName::from_static("www-authenticate");

/// What the TLS layer learned about the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PeerIdentity {
    /// A plain `ws://` connection.
    Plain,
    /// TLS without a client certificate.
    NoCertificate,
    /// TLS with a (chain-verified) client certificate; the subject's Common Name, if it has one.
    Certificate { common_name: Option<String> },
}

impl PeerIdentity {
    /// The identity of an accepted TLS connection, from the DER of the client's leaf.
    pub(crate) fn from_leaf(leaf_der: Option<&[u8]>) -> Self {
        match leaf_der {
            None => Self::NoCertificate,
            Some(der) => Self::Certificate {
                common_name: common_name(der),
            },
        }
    }
}

/// How a refusal is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Denied {
    /// Fixed phrase for [`crate::CsmsEvent::AuthFailed`].
    pub reason: &'static str,
    /// Basic Auth problem (401 with a challenge) rather than a certificate problem (403).
    pub basic: bool,
    /// The challenge came with a binary key allowed, so it must not promise UTF-8.
    pub binary_key: bool,
}

impl Denied {
    fn tls(reason: &'static str) -> Self {
        Self {
            reason,
            basic: false,
            binary_key: false,
        }
    }

    /// The HTTP refusal. The body never says why.
    pub(crate) fn response(self) -> ErrorResponse {
        let mut response = ErrorResponse::new(Some("unauthorized".to_owned()));
        if self.basic {
            *response.status_mut() = StatusCode::UNAUTHORIZED;
            let challenge = if self.binary_key {
                "Basic realm=\"kirby-csms\""
            } else {
                "Basic realm=\"kirby-csms\", charset=\"UTF-8\""
            };
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static(challenge));
        } else {
            *response.status_mut() = StatusCode::FORBIDDEN;
        }
        response
    }
}

/// Checks a handshake request for station `id` against `policy`. `binary_key` allows the
/// decoded-hex form of the Basic Auth key; only a session that ends up as OCPP 1.6 may use it.
pub(crate) fn check(
    policy: &SecurityPolicy,
    request: &Request,
    id: &str,
    peer: &PeerIdentity,
    binary_key: bool,
) -> Result<(), Denied> {
    match policy.profile {
        SecurityProfile::None => Ok(()),
        SecurityProfile::BasicAuth | SecurityProfile::BasicAuthTls => {
            if policy.profile == SecurityProfile::BasicAuthTls && *peer == PeerIdentity::Plain {
                return Err(Denied::tls("TLS required"));
            }
            check_basic(policy, request, id, binary_key)
        }
        SecurityProfile::MutualTls => match peer {
            PeerIdentity::Plain => Err(Denied::tls("TLS required")),
            PeerIdentity::NoCertificate => Err(Denied::tls("client certificate required")),
            PeerIdentity::Certificate { common_name } => {
                if common_name.as_deref() == Some(id) {
                    Ok(())
                } else {
                    Err(Denied::tls(
                        "client certificate does not match the station id",
                    ))
                }
            }
        },
    }
}

fn check_basic(
    policy: &SecurityPolicy,
    request: &Request,
    id: &str,
    binary_key: bool,
) -> Result<(), Denied> {
    let denied = |reason| Denied {
        reason,
        basic: true,
        binary_key,
    };
    let Some((user, password)) = credentials(request) else {
        return Err(denied("missing or malformed credentials"));
    };
    let key = policy.key_for(id);
    // All comparisons always run, so timing does not tell which part was wrong.
    let user_ok = constant_time_eq(&user, id.as_bytes());
    let literal_ok = constant_time_eq(&password, key.unwrap_or_default().as_bytes());
    // OCPP-J 1.6 sends a key of 40 hex digits as its 20 decoded bytes.
    let decoded = key.filter(|_| binary_key).and_then(decode_hex);
    let decoded_ok =
        constant_time_eq(&password, decoded.as_deref().unwrap_or_default()) & decoded.is_some();
    if key.is_none() {
        Err(denied("no key configured for the station"))
    } else if user_ok && (literal_ok | decoded_ok) {
        Ok(())
    } else {
        Err(denied("invalid credentials"))
    }
}

/// The bytes of a nonempty, even-length hex string (either case).
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let digits = text.as_bytes();
    if digits.is_empty() || !digits.len().is_multiple_of(2) {
        return None;
    }
    digits
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = char::from(pair[0]).to_digit(16)?;
            let low = char::from(pair[1]).to_digit(16)?;
            u8::try_from(high << 4 | low).ok()
        })
        .collect()
}

/// The user and password bytes of a `Authorization: Basic` header, split at the first colon.
/// The password is not required to be UTF-8 (OCPP-J 1.6 sends a binary key).
fn credentials(request: &Request) -> Option<(Vec<u8>, Vec<u8>)> {
    let value = request.headers().get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, encoded) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = STANDARD.decode(encoded.trim()).ok()?;
    let colon = decoded.iter().position(|&byte| byte == b':')?;
    Some((decoded[..colon].to_vec(), decoded[colon + 1..].to_vec()))
}

/// Compares without stopping at the first difference (the length is compared too).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        let left = a.get(i).copied().unwrap_or(0);
        let right = b.get(i).copied().unwrap_or(0);
        diff |= usize::from(left ^ right);
    }
    std::hint::black_box(diff) == 0
}

fn common_name(der: &[u8]) -> Option<String> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).ok()?;
    cert.subject()
        .iter_common_name()
        .next()?
        .as_str()
        .ok()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_covers_length_and_content() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abc\0", b"abc"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn empty_key_is_not_configured() {
        let policy = SecurityPolicy {
            profile: SecurityProfile::BasicAuth,
            keys: [("A".to_owned(), String::new())].into(),
            default_key: Some(String::new()),
        };
        assert_eq!(policy.key_for("A"), None);
        assert_eq!(policy.key_for("B"), None);
        assert!(policy.has_empty_key());
    }
}
