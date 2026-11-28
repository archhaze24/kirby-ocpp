//! Small browser helpers: storage that may be unavailable, timers, time text, file download.

use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

fn storage(session: bool) -> Option<web_sys::Storage> {
    let window = web_sys::window()?;
    if session {
        window.session_storage().ok().flatten()
    } else {
        window.local_storage().ok().flatten()
    }
}

/// Reads a stored value; a blocked or empty storage gives `None`.
pub fn stored(session: bool, key: &str) -> Option<String> {
    storage(session)?.get_item(key).ok().flatten()
}

/// Stores a value (`None` removes it); failures are ignored, the UI works without storage.
pub fn store(session: bool, key: &str, value: Option<&str>) {
    let Some(storage) = storage(session) else {
        return;
    };
    let _ = match value {
        Some(value) => storage.set_item(key, value),
        None => storage.remove_item(key),
    };
}

/// Waits `ms` milliseconds.
pub async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(promise).await;
}

/// The current time as `2026-01-31T12:00:00Z` (whole seconds), the form of the timestamp
/// fields.
pub fn now_rfc3339() -> String {
    let iso = String::from(js_sys::Date::new_0().to_iso_string());
    match iso.split_once('.') {
        Some((head, _)) => format!("{head}Z"),
        None => iso,
    }
}

fn parse_date(ts: &str) -> Option<js_sys::Date> {
    let date = js_sys::Date::new(&JsValue::from_str(ts));
    (!date.get_time().is_nan()).then_some(date)
}

/// Local `HH:MM:SS.mmm` of a timestamp; the text itself when it is not a date.
pub fn clock_time(ts: &str) -> String {
    parse_date(ts).map_or_else(
        || ts.to_owned(),
        |d| {
            format!(
                "{:02}:{:02}:{:02}.{:03}",
                d.get_hours(),
                d.get_minutes(),
                d.get_seconds(),
                d.get_milliseconds()
            )
        },
    )
}

/// Local `YYYY-MM-DD HH:MM:SS` of a timestamp; the text itself when it is not a date.
pub fn date_time(ts: &str) -> String {
    parse_date(ts).map_or_else(
        || ts.to_owned(),
        |d| {
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                d.get_full_year(),
                d.get_month() + 1,
                d.get_date(),
                d.get_hours(),
                d.get_minutes(),
                d.get_seconds()
            )
        },
    )
}

/// Gives the browser `text` as a file to save.
pub fn download(name: &str, mime: &str, text: &str) {
    let parts = js_sys::Array::of1(&JsValue::from_str(text));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(mime);
    let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options) else {
        return;
    };
    let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
        return;
    };
    if let Some(document) = web_sys::window().and_then(|w| w.document())
        && let Ok(element) = document.create_element("a")
        && let Ok(anchor) = element.dyn_into::<web_sys::HtmlAnchorElement>()
    {
        anchor.set_href(&url);
        anchor.set_download(name);
        anchor.click();
    }
    let _ = web_sys::Url::revoke_object_url(&url);
}

/// Pretty JSON text of a value.
pub fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// Removes control and bidirectional characters from text that came from a station before it is
/// shown. The text still goes into the page as text, never as HTML; this keeps lookalike and
/// reordering tricks out of ids and names.
pub fn clean(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control()
                || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                '\u{fffd}'
            } else {
                c
            }
        })
        .collect()
}
