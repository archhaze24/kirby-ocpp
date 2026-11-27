//! Authentication: the bearer token, and the loopback rules that apply without one.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

use super::ApiState;
use super::error::error;
use super::web;

/// Equality that does not stop at the first differing byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for (index, byte) in b.iter().enumerate() {
        diff |= usize::from(byte ^ a.get(index).copied().unwrap_or(0));
    }
    diff == 0
}

/// Whether a `Host` header value names this machine: a loopback IP literal (`127.0.0.1`,
/// `[::1]`, `[0:0:0:0:0:0:0:1]`), the address the API is bound to, or `localhost` (with an
/// optional trailing dot), each with an optional port. Anything else (a DNS name that
/// rebinds to 127.0.0.1) is not.
fn is_loopback_host(value: &str, bound: IpAddr) -> bool {
    let known = |ip: IpAddr| ip.is_loopback() || (ip == bound && !ip.is_unspecified());
    let (host_ok, port) = if let Some(rest) = value.strip_prefix('[') {
        let Some((ip, tail)) = rest.split_once(']') else {
            return false;
        };
        (
            ip.parse::<Ipv6Addr>().is_ok_and(|ip| known(IpAddr::V6(ip))),
            tail,
        )
    } else {
        let (host, port) = value.split_at(value.find(':').unwrap_or(value.len()));
        let named =
            host.eq_ignore_ascii_case("localhost") || host.eq_ignore_ascii_case("localhost.");
        (
            named
                || host
                    .parse::<Ipv4Addr>()
                    .is_ok_and(|ip| known(IpAddr::V4(ip))),
            port,
        )
    };
    host_ok && (port.is_empty() || valid_port(port))
}

/// `:` and up to five digits.
fn valid_port(text: &str) -> bool {
    text.strip_prefix(':').is_some_and(|port| {
        (1..=5).contains(&port.len()) && port.bytes().all(|b| b.is_ascii_digit())
    })
}

/// Whether a request comes from the web UI page this listener served: its `Origin` is exactly
/// `<scheme>://<Host>` (compared case-insensitively), the `Host` names this machine and the
/// browser says the request is `same-origin` (or sends no `Sec-Fetch-Site`).
fn from_own_page(headers: &HeaderMap, state: &ApiState) -> bool {
    let (Some(origin), Some(host)) = (
        headers.get(header::ORIGIN),
        headers
            .get(header::HOST)
            .and_then(|host| host.to_str().ok()),
    ) else {
        return false;
    };
    let scheme = if state.https { "https" } else { "http" };
    let own = format!("{scheme}://{host}");
    state.web
        && origin.as_bytes().eq_ignore_ascii_case(own.as_bytes())
        && is_loopback_host(host, state.bound)
        && headers
            .get("sec-fetch-site")
            .is_none_or(|value| value.as_bytes().eq_ignore_ascii_case(b"same-origin"))
}

/// With a token every request needs exactly one `Authorization: Bearer TOKEN`; the pages of
/// the web UI are the exception, they hold no data. Without a token the API trusts the
/// loopback address only, so a browser must not be able to reach it: a request with an
/// `Origin` header, a `Sec-Fetch-Site: cross-site` header or a `Host` that is not a loopback
/// name (DNS rebinding) is refused. With the web UI on, a request from its own page (see
/// [`from_own_page`]) passes the `Origin` check.
pub(super) async fn auth(State(state): State<ApiState>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    if headers.get_all(header::HOST).iter().nth(1).is_some() {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "more than one Host header",
        );
    }
    if state.token.is_some() && state.web && web::is_asset(request.uri().path()) {
        return next.run(request).await;
    }
    if let Some(token) = &state.token {
        let mut values = headers.get_all(header::AUTHORIZATION).iter();
        let given = match (values.next(), values.next()) {
            (Some(value), None) => value
                .to_str()
                .ok()
                .and_then(|value| value.split_once(' '))
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                .map(|(_, given)| given),
            _ => None,
        };
        if !given.is_some_and(|given| constant_time_eq(given.as_bytes(), token.as_bytes())) {
            let mut response = error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "missing or wrong bearer token",
            );
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            return response;
        }
    } else if headers.contains_key(header::ORIGIN) && !from_own_page(headers, &state) {
        return error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "requests with an Origin header are refused without an API token",
        );
    } else if headers
        .get("sec-fetch-site")
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"cross-site"))
    {
        return error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "cross-site requests are refused without an API token",
        );
    } else if headers.get(header::HOST).is_some_and(|host| {
        !host
            .to_str()
            .is_ok_and(|host| is_loopback_host(host, state.bound))
    }) {
        return error(
            StatusCode::FORBIDDEN,
            "forbidden",
            &format!(
                "without an API token the Host header must be 127.0.0.1, [::1], localhost \
                 (any loopback IP) or the API address {}, with an optional port",
                state.bound
            ),
        );
    }
    next.run(request).await
}
