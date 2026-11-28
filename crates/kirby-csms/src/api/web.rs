//! The web UI (`--web`): the files of `web/dist/` embedded in the binary by `build.rs`, served
//! on the API listener. They are not part of `/v1`: no route table entry, no OpenAPI path.
//! Without a built `dist/` the table is empty and `/` answers with a page that names the build
//! command.

use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

/// What a page of the UI may load and talk to: its own origin only. `'wasm-unsafe-eval'`
/// allows compiling the WebAssembly module and nothing else.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; \
style-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'none'; \
frame-ancestors 'none'";

/// The files of `web/dist/`: served path (with the leading `/`) and content, sorted by path.
static EMBEDDED: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

/// The page of a binary built without `web/dist/`.
const NOT_BUILT: &str = "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
<title>Web UI not built</title>\n</head>\n<body>\n<h1>Web UI not built</h1>\n\
<p>This binary was built without the web UI. To include it, run <code>trunk build --release</code> \
in <code>crates/kirby-csms-web</code>, then build <code>kirby-csms</code> again.</p>\n\
<p>The HTTP API at <code>/v1</code> works without it.</p>\n</body>\n</html>\n";

/// The two paths that always serve the start page.
const PAGES: [&str; 2] = ["/", "/index.html"];

/// The content type of a served file, by extension.
fn content_type(path: &str) -> &'static str {
    let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
    match extension.to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

/// The embedded content of `path` (`/` and `/index.html` fall back to the built-in page).
fn lookup(path: &str) -> Option<(&'static str, &'static [u8])> {
    let embedded = |wanted: &str| {
        EMBEDDED
            .iter()
            .find(|(served, _)| *served == wanted)
            .map(|(served, body)| (content_type(served), *body))
    };
    if PAGES.contains(&path) {
        return embedded("/index.html")
            .or(Some((content_type("index.html"), NOT_BUILT.as_bytes())));
    }
    embedded(path)
}

/// Every path of the web UI.
pub(super) fn paths() -> impl Iterator<Item = &'static str> {
    PAGES.into_iter().chain(
        EMBEDDED
            .iter()
            .map(|(served, _)| *served)
            .filter(|served| !PAGES.contains(served)),
    )
}

/// Whether `path` is a page or file of the web UI.
pub(super) fn is_asset(path: &str) -> bool {
    paths().any(|known| known == path)
}

/// Serves the asset the request path names. Routes exist only for these paths.
pub(super) async fn asset(request: Request) -> Response {
    let Some((content_type, body)) = lookup(request.uri().path()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
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
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types_follow_the_extension() {
        for (path, expected) in [
            ("/index.html", "text/html; charset=utf-8"),
            ("/boot.js", "text/javascript; charset=utf-8"),
            ("/kirby-csms-web_bg.wasm", "application/wasm"),
            ("/style.css", "text/css; charset=utf-8"),
            ("/favicon.svg", "image/svg+xml"),
            ("/manifest.json", "application/json"),
            ("/Upper.WASM", "application/wasm"),
            ("/dir.d/data.bin", "application/octet-stream"),
            ("/no-extension", "application/octet-stream"),
        ] {
            assert_eq!(content_type(path), expected, "{path}");
        }
    }

    #[test]
    fn the_start_page_exists_with_or_without_a_build() {
        for path in PAGES {
            let (content_type, body) = lookup(path).expect("start page");
            assert_eq!(content_type, "text/html; charset=utf-8");
            assert_ne!(body, b"");
        }
        assert!(lookup("/no/such/file").is_none());
    }

    #[test]
    fn embedded_paths_are_absolute_unique_and_outside_the_api() {
        let mut seen = std::collections::HashSet::new();
        for path in paths() {
            assert!(path.starts_with('/'), "{path}");
            assert!(!path.starts_with("/v1"), "{path}");
            assert!(seen.insert(path), "{path} twice");
            assert!(is_asset(path));
        }
        assert!(!is_asset("/v1/health"));
    }
}
