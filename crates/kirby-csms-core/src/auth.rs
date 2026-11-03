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
}

impl Denied {
    /// The HTTP refusal. The body never says why.
    pub(crate) fn response(self) -> ErrorResponse {
        let mut response = ErrorResponse::new(Some("unauthorized".to_owned()));
        if self.basic {
            *response.status_mut() = StatusCode::UNAUTHORIZED;
            response.headers_mut().insert(
                WWW_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"kirby-csms\", charset=\"UTF-8\""),
            );
        } else {
            *response.status_mut() = StatusCode::FORBIDDEN;
        }
        response
    }
}

/// Checks a handshake request for station `id` against `policy`.
pub(crate) fn check(
    policy: &SecurityPolicy,
    request: &Request,
    id: &str,
    peer: &PeerIdentity,
) -> Result<(), Denied> {
    match policy.profile {
        SecurityProfile::None => Ok(()),
        SecurityProfile::BasicAuth | SecurityProfile::BasicAuthTls => {
            if policy.profile == SecurityProfile::BasicAuthTls && *peer == PeerIdentity::Plain {
                return Err(Denied {
                    reason: "TLS required",
                    basic: false,
                });
            }
            check_basic(policy, request, id)
        }
        SecurityProfile::MutualTls => match peer {
            PeerIdentity::Plain => Err(Denied {
                reason: "TLS required",
                basic: false,
            }),
            PeerIdentity::NoCertificate => Err(Denied {
                reason: "client certificate required",
                basic: false,
            }),
            PeerIdentity::Certificate { common_name } => {
                if common_name.as_deref() == Some(id) {
                    Ok(())
                } else {
                    Err(Denied {
                        reason: "client certificate does not match the station id",
                        basic: false,
                    })
                }
            }
        },
    }
}

fn check_basic(policy: &SecurityPolicy, request: &Request, id: &str) -> Result<(), Denied> {
    let denied = |reason| Denied {
        reason,
        basic: true,
    };
    let Some((user, password)) = credentials(request) else {
        return Err(denied("missing or malformed credentials"));
    };
    let key = policy.key_for(id);
    // Both comparisons always run, so timing does not tell which part was wrong.
    let user_ok = constant_time_eq(user.as_bytes(), id.as_bytes());
    let key_ok = constant_time_eq(password.as_bytes(), key.unwrap_or_default().as_bytes());
    if key.is_none() {
        Err(denied("no key configured for the station"))
    } else if user_ok & key_ok {
        Ok(())
    } else {
        Err(denied("invalid credentials"))
    }
}

/// The user and password of a `Authorization: Basic` header.
fn credentials(request: &Request) -> Option<(String, String)> {
    let value = request.headers().get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, encoded) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = STANDARD.decode(encoded.trim()).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (user, password) = text.split_once(':')?;
    Some((user.to_owned(), password.to_owned()))
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
}
