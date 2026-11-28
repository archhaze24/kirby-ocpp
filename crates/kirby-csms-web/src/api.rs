//! The `/v1` shapes the UI reads, the fetch helpers and the event stream over `fetch`
//! (`EventSource` cannot send an `Authorization` header). Unknown fields are ignored, as the
//! API contract asks.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{AbortSignal, Headers, ReadableStreamDefaultReader, RequestInit, Response};

// ---- shapes ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Station {
    pub id: String,
    pub connected: bool,
    pub protocol: Option<String>,
    pub received_calls: u64,
    pub remote_addr: Option<String>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub connectors: BTreeMap<String, String>,
    pub active_transactions: Vec<ActiveTx>,
    pub security_events: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ActiveTx {
    pub id: String,
    pub connector: Option<u32>,
    pub evse: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StationDetail {
    #[serde(flatten)]
    pub station: Station,
    pub state: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Transaction {
    pub station: String,
    pub id: String,
    pub connector: Option<u32>,
    pub id_tag: Option<String>,
    pub id_tag_status: Option<String>,
    pub meter_start: Option<i64>,
    pub started_at: Option<String>,
    pub status: String,
    pub meter_stop: Option<i64>,
    pub stopped_at: Option<String>,
    pub stop_reason: Option<String>,
    pub energy_wh: Option<f64>,
    pub power_w: Option<f64>,
    pub last_meter_at: Option<String>,
    pub evse: Option<u32>,
    pub tariff_id: Option<String>,
    pub cost: Option<Cost>,
    pub priority_charging: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Cost {
    pub currency: String,
    pub total_excl_tax: Option<f64>,
    pub total_incl_tax: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TransactionList {
    pub transactions: Vec<Transaction>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Health {
    pub version: String,
    pub stations: u64,
}

/// A recorded frame, from `/history` or a `frame` event.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Frame {
    pub ts: String,
    pub station: String,
    pub direction: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct History {
    pub frames: Vec<Frame>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ActionList {
    pub protocol: String,
    pub actions: Vec<ActionSpec>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ActionSpec {
    pub name: String,
    pub title: String,
    pub summary: String,
    pub group: String,
    /// `form`, `raw` or `none`.
    pub input: String,
    pub fields: Vec<FieldSpec>,
    pub template: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct FieldSpec {
    pub name: String,
    pub label: String,
    /// `text`, `integer`, `choice`, `json`, `pem`, `timestamp`, `boolean` or `transaction`.
    #[serde(rename = "type")]
    pub ty: String,
    pub choices: Vec<String>,
    pub default: Option<String>,
    pub required: bool,
    pub hint: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FaultList {
    pub remaining: u64,
    pub faults: Vec<Fault>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Fault {
    pub id: String,
    pub scope: String,
    pub station: Option<String>,
    pub protocol: Option<String>,
    pub action: String,
    pub kind: String,
    pub remaining: u64,
}

/// The body of `POST /v1/faults`.
#[derive(Debug, Clone, Serialize)]
pub struct NewFault {
    pub action: String,
    pub kind: String,
    pub times: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub station: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

/// One server-sent event.
#[derive(Debug, Clone, Default)]
pub struct SseEvent {
    pub id: Option<String>,
    pub event: String,
    pub data: String,
}

// ---- transport ------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// `401`: the API wants a token (or the one given is wrong).
    Unauthorized,
    /// Any other error status, with the API's `error.code` and `error.message` when present.
    Http {
        status: u16,
        code: String,
        message: String,
    },
    /// The request did not reach the server or the answer was unusable.
    Network(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("unauthorized: the API token is missing or wrong"),
            Self::Http {
                status,
                code,
                message,
            } => write!(f, "{status} {code}: {message}"),
            Self::Network(text) => write!(f, "network error: {text}"),
        }
    }
}

fn js_text(value: &JsValue) -> String {
    value
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(value, &JsValue::from_str("message"))
                .ok()
                .and_then(|message| message.as_string())
        })
        .unwrap_or_else(|| "request failed".to_owned())
}

fn headers(token: Option<&str>, accept: &str, extra: &[(&str, &str)]) -> Result<Headers, ApiError> {
    let map = Headers::new().map_err(|e| ApiError::Network(js_text(&e)))?;
    let set = |name: &str, value: &str| {
        map.set(name, value)
            .map_err(|e| ApiError::Network(js_text(&e)))
    };
    set("Accept", accept)?;
    if let Some(token) = token {
        set("Authorization", &format!("Bearer {token}"))?;
    }
    for (name, value) in extra {
        set(name, value)?;
    }
    Ok(map)
}

async fn fetch(
    method: &str,
    path: &str,
    body: Option<&str>,
    token: Option<&str>,
    accept: &str,
    extra: &[(&str, &str)],
    signal: Option<&AbortSignal>,
) -> Result<Response, ApiError> {
    let window = web_sys::window().ok_or_else(|| ApiError::Network("no window".to_owned()))?;
    let init = RequestInit::new();
    init.set_method(method);
    init.set_headers(&JsValue::from(headers(token, accept, extra)?));
    if let Some(body) = body {
        init.set_body(&JsValue::from_str(body));
    }
    if let Some(signal) = signal {
        init.set_signal(Some(signal));
    }
    let response = JsFuture::from(window.fetch_with_str_and_init(path, &init))
        .await
        .map_err(|e| ApiError::Network(js_text(&e)))?;
    response
        .dyn_into::<Response>()
        .map_err(|_| ApiError::Network("not a response".to_owned()))
}

/// The error of a non-2xx answer; the body is the API's error object when it is one.
async fn error_of(response: &Response) -> ApiError {
    let status = response.status();
    if status == 401 {
        return ApiError::Unauthorized;
    }
    let text = match response.text() {
        Ok(promise) => JsFuture::from(promise)
            .await
            .ok()
            .and_then(|text| text.as_string())
            .unwrap_or_default(),
        Err(_) => String::new(),
    };
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let text_of = |key: &str| parsed["error"][key].as_str().unwrap_or_default().to_owned();
    ApiError::Http {
        status,
        code: text_of("code"),
        message: if parsed["error"]["message"].is_string() {
            text_of("message")
        } else {
            format!("HTTP {status}")
        },
    }
}

/// One request; returns the body text of a 2xx answer.
pub async fn request(
    method: &str,
    path: &str,
    body: Option<&str>,
    token: Option<&str>,
) -> Result<String, ApiError> {
    let extra: &[(&str, &str)] = if body.is_some() {
        &[("Content-Type", "application/json")]
    } else {
        &[]
    };
    let response = fetch(method, path, body, token, "application/json", extra, None).await?;
    if !response.ok() {
        return Err(error_of(&response).await);
    }
    let text = response
        .text()
        .map_err(|e| ApiError::Network(js_text(&e)))?;
    JsFuture::from(text)
        .await
        .map_err(|e| ApiError::Network(js_text(&e)))?
        .as_string()
        .ok_or_else(|| ApiError::Network("the answer is not text".to_owned()))
}

/// A request whose answer is JSON of type `T`.
pub async fn request_json<T: for<'de> Deserialize<'de>>(
    method: &str,
    path: &str,
    body: Option<&str>,
    token: Option<&str>,
) -> Result<T, ApiError> {
    let text = request(method, path, body, token).await?;
    serde_json::from_str(&text).map_err(|e| ApiError::Network(format!("unexpected answer: {e}")))
}

/// Percent-encodes `text` for a path segment or a query value.
pub fn encode(text: &str) -> String {
    String::from(js_sys::encode_uri_component(text))
}

// ---- event stream ---------------------------------------------------------------------------

/// Reads `/v1/events` until the stream ends, `on_event` is told to stop (it returns `false`) or
/// the request fails. `last_id` resumes after that event id.
pub async fn stream_events(
    token: Option<&str>,
    last_id: Option<&str>,
    signal: &AbortSignal,
    mut on_event: impl FnMut(SseEvent) -> bool,
    mut on_open: impl FnMut(),
) -> Result<(), ApiError> {
    let extra: Vec<(&str, &str)> = last_id
        .map(|id| ("Last-Event-ID", id))
        .into_iter()
        .collect();
    let response = fetch(
        "GET",
        "/v1/events",
        None,
        token,
        "text/event-stream",
        &extra,
        Some(signal),
    )
    .await?;
    if !response.ok() {
        return Err(error_of(&response).await);
    }
    let body = response
        .body()
        .ok_or_else(|| ApiError::Network("no response body".to_owned()))?;
    let reader: ReadableStreamDefaultReader = body
        .get_reader()
        .dyn_into()
        .map_err(|_| ApiError::Network("no stream reader".to_owned()))?;
    on_open();
    let mut parser = SseParser::default();
    loop {
        let chunk = JsFuture::from(reader.read())
            .await
            .map_err(|e| ApiError::Network(js_text(&e)))?;
        let done = js_sys::Reflect::get(&chunk, &JsValue::from_str("done"))
            .ok()
            .and_then(|done| done.as_bool())
            .unwrap_or(true);
        if done {
            return Ok(());
        }
        let value = js_sys::Reflect::get(&chunk, &JsValue::from_str("value"))
            .map_err(|e| ApiError::Network(js_text(&e)))?;
        let bytes = js_sys::Uint8Array::new(&value).to_vec();
        for event in parser.feed(&bytes) {
            if !on_event(event) {
                // Cancel quietly: the caller no longer wants the stream.
                let _ = reader.cancel();
                return Ok(());
            }
        }
    }
}

/// Splits the byte stream into SSE messages. Lines are decoded one at a time, so a multi-byte
/// character cut by a chunk boundary stays whole.
#[derive(Debug, Default)]
pub struct SseParser {
    pending: Vec<u8>,
    current: SseEvent,
    has_data: bool,
}

impl SseParser {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if let Some(event) = self.line(line) {
                events.push(event);
            }
        }
        events
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            let event = std::mem::take(&mut self.current);
            let has_data = std::mem::take(&mut self.has_data);
            return (has_data || !event.event.is_empty()).then_some(event);
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "id" => self.current.id = Some(value.to_owned()),
            "event" => value.clone_into(&mut self.current.event),
            "data" => {
                if self.has_data {
                    self.current.data.push('\n');
                }
                self.current.data.push_str(value);
                self.has_data = true;
            }
            _ => {}
        }
        None
    }
}
