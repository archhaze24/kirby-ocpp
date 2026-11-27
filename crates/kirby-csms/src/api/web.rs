//! The web UI (`--web`): four static files embedded in the binary and served on the API
//! listener. They are not part of `/v1`: no route table entry, no OpenAPI path.

use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

/// What a page of the UI may load and talk to: its own origin only.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'none'; \
frame-ancestors 'none'";

/// One embedded file and the paths it is served at.
struct Asset {
    paths: &'static [&'static str],
    content_type: &'static str,
    body: &'static str,
}

const ASSETS: &[Asset] = &[
    Asset {
        paths: &["/", "/index.html"],
        content_type: "text/html; charset=utf-8",
        body: include_str!("../../web/index.html"),
    },
    Asset {
        paths: &["/assets/app.js"],
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../../web/app.js"),
    },
    Asset {
        paths: &["/assets/style.css"],
        content_type: "text/css; charset=utf-8",
        body: include_str!("../../web/style.css"),
    },
    Asset {
        paths: &["/assets/favicon.svg"],
        content_type: "image/svg+xml",
        body: include_str!("../../web/favicon.svg"),
    },
];

/// Every path of the web UI.
pub(super) fn paths() -> impl Iterator<Item = &'static str> {
    ASSETS.iter().flat_map(|asset| asset.paths.iter().copied())
}

/// Whether `path` is a page or file of the web UI.
pub(super) fn is_asset(path: &str) -> bool {
    paths().any(|known| known == path)
}

/// Serves the asset the request path names. Routes exist only for these paths.
pub(super) async fn asset(request: Request) -> Response {
    let path = request.uri().path();
    let Some(asset) = ASSETS.iter().find(|asset| asset.paths.contains(&path)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(asset.content_type),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(CONTENT_SECURITY_POLICY),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        asset.body,
    )
        .into_response()
}
