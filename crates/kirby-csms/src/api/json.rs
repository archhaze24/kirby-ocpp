//! Request parsing shared by the handlers: path ids, JSON bodies, query parameters, and the
//! shaping of transaction ids in responses.

use axum::body::Body;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Request};
use axum::http::{StatusCode, Uri, header};
use axum::response::Response;
use futures_util::StreamExt;
use kirby_ocpp_core::text::is_unsafe_char;
use serde_json::Value;

use super::error::error;
use super::limits::{BODY_TIMEOUT, MAX_BODY_BYTES};

/// Turns the integer `id` of every transaction object into a string: transaction ids are
/// integers in OCPP 1.6 and strings in 2.x, and the API serves strings for both. Applies to
/// a station object (`active_transactions`, `state.transactions`) and a `state` event.
pub(super) fn stringify_transaction_ids(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if let Some(Value::Object(state)) = object.get_mut("state") {
        state.get_mut("transactions").map(stringify_list);
    }
    object.get_mut("active_transactions").map(stringify_list);
}

fn stringify_list(list: &mut Value) {
    let Value::Array(items) = list else {
        return;
    };
    for item in items {
        if let Some(Value::Number(id)) = item.get("id").cloned()
            && let Some(object) = item.as_object_mut()
        {
            object.insert("id".to_owned(), id.to_string().into());
        }
    }
}

/// The decoded station id of the path, refused when it holds control or bidi characters.
pub(super) fn station_id(id: Result<Path<String>, PathRejection>) -> Result<String, Box<Response>> {
    let Ok(Path(id)) = id else {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "invalid station id in the path",
        )));
    };
    if id.chars().any(is_unsafe_char) {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "station id has control or bidirectional characters",
        )));
    }
    Ok(id)
}

/// The request body (at most 1 MiB, read within 10 s) parsed as JSON.
pub(super) async fn read_json(request: Request<Body>) -> Result<(Value, usize), Box<Response>> {
    let too_large = || {
        Box::new(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "request body is larger than 1 MiB",
        ))
    };
    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|length| length > MAX_BODY_BYTES) {
        return Err(too_large());
    }
    let collect = async {
        let mut chunks = request.into_body().into_data_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| {
                Box::new(error(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "cannot read the request body",
                ))
            })?;
            if bytes.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
                return Err(too_large());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    };
    let bytes = match tokio::time::timeout(BODY_TIMEOUT, collect).await {
        Ok(bytes) => bytes?,
        Err(_) => {
            return Err(Box::new(error(
                StatusCode::REQUEST_TIMEOUT,
                "request_timeout",
                "the request body did not arrive in 10 s",
            )));
        }
    };
    let length = bytes.len();
    serde_json::from_slice(&bytes)
        .map(|value| (value, length))
        .map_err(|e| {
            Box::new(error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                &format!("invalid JSON: {e}"),
            ))
        })
}

/// The value of the query parameter `name`, percent-decoded (`%XX` only; `+` stays a plus).
/// Any other parameter, and a repeated one, is an error.
pub(super) fn query_param(uri: &Uri, name: &str) -> Result<Option<String>, String> {
    let Some(query) = uri.query() else {
        return Ok(None);
    };
    let mut found = None;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        match pair.split_once('=') {
            Some((key, _)) if key == name && found.is_some() => {
                return Err(format!("repeated query parameter `{name}`"));
            }
            Some((key, value)) if key == name => found = Some(percent_decode(value)),
            _ => return Err(format!("unknown query parameter; supported: {name}")),
        }
    }
    Ok(found)
}

/// The query parameters of the request, percent-decoded, as pairs in the order of `names`
/// (`None` for a parameter that was not sent). A parameter that is not in `names`, and a
/// repeated one, is an error.
pub(super) fn query_params<const N: usize>(
    uri: &Uri,
    names: [&str; N],
) -> Result<[Option<String>; N], String> {
    let mut found: [Option<String>; N] = std::array::from_fn(|_| None);
    let Some(query) = uri.query() else {
        return Ok(found);
    };
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let Some(slot) = names.iter().position(|name| *name == key) else {
            return Err(format!(
                "unknown query parameter; supported: {}",
                names.join(", ")
            ));
        };
        if pair.split_once('=').is_none() {
            return Err(format!("query parameter `{key}` needs a value"));
        }
        let Some(cell) = found.get_mut(slot) else {
            continue;
        };
        if cell.is_some() {
            return Err(format!("repeated query parameter `{key}`"));
        }
        *cell = Some(percent_decode(value));
    }
    Ok(found)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let hex = |offset: usize| {
            bytes
                .get(index + offset)
                .and_then(|digit| char::from(*digit).to_digit(16))
        };
        if byte == b'%'
            && let (Some(high), Some(low)) = (hex(1), hex(2))
        {
            out.push(u8::try_from(high * 16 + low).unwrap_or(b'?'));
            index += 3;
        } else {
            out.push(byte);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
