//! The requests `ctl` sends, shaped by `docs/kirby-csms-openapi.json`. Only fields that are
//! given are sent: the API refuses unknown fields on some bodies and `null` on others.

use clap::Args;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Map, Value, json};

/// Everything but the unreserved characters is percent-encoded, in paths and query values.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn enc(text: &str) -> String {
    utf8_percent_encode(text, COMPONENT).to_string()
}

/// How the answer is printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Json,
    Text,
}

/// One API request.
#[derive(Debug)]
pub struct Spec {
    pub method: &'static str,
    /// The route as written in the OpenAPI document.
    #[allow(dead_code, reason = "the tests check it against the OpenAPI document")]
    pub route: &'static str,
    /// Path and query, encoded.
    pub path: String,
    pub body: Option<Value>,
    pub kind: Kind,
}

impl Spec {
    fn new(method: &'static str, route: &'static str, path: String, kind: Kind) -> Self {
        Self {
            method,
            route,
            path,
            body: None,
            kind,
        }
    }

    fn json(method: &'static str, route: &'static str, path: &str) -> Self {
        Self::new(method, route, path.to_owned(), Kind::Json)
    }

    fn with_body(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }
}

fn with_query(path: &str, pairs: &[(&str, Option<String>)]) -> String {
    let mut out = path.to_owned();
    let mut sep = '?';
    for (name, value) in pairs {
        if let Some(value) = value {
            out.push(sep);
            out.push_str(name);
            out.push('=');
            out.push_str(&enc(value));
            sep = '&';
        }
    }
    out
}

/// A history cursor: `c` and decimal digits.
pub fn parse_cursor(text: &str) -> Result<String, String> {
    let digits = text.strip_prefix('c').unwrap_or_default();
    if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(text.to_owned())
    } else {
        Err("expected a cursor such as c42 (the next_cursor of an earlier answer)".to_owned())
    }
}

fn parse_since_time(text: &str) -> Result<String, String> {
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|_| text.to_owned())
        .map_err(|_| "expected an RFC 3339 time such as 2030-01-01T10:00:00Z".to_owned())
}

fn non_empty(text: &str) -> Result<String, String> {
    if text.is_empty() {
        Err("must not be empty".to_owned())
    } else {
        Ok(text.to_owned())
    }
}

/// Parses `--timeout-ms`: 1 to 86400000.
pub fn parse_millis(text: &str) -> Result<u32, String> {
    let value: u32 = text.parse().map_err(|_| "expected a number".to_owned())?;
    if (1..=86_400_000).contains(&value) {
        Ok(value)
    } else {
        Err("expected 1 to 86400000".to_owned())
    }
}

/// Filters of the history routes.
#[derive(Debug, Args)]
pub struct HistoryFilter {
    /// Number of frames (the newest ones, or the first ones after `--since`)
    #[arg(long, value_name = "COUNT")]
    pub limit: Option<u32>,
    /// Only frames after this cursor (the `next_cursor` of an earlier answer)
    #[arg(long, value_name = "CURSOR", value_parser = parse_cursor)]
    pub since: Option<String>,
    /// Only frames of CALLs of this action and the replies to them
    #[arg(long, value_name = "ACTION", value_parser = non_empty)]
    pub action: Option<String>,
    /// Only frames of this kind
    #[arg(long, value_name = "KIND", value_parser = ["call", "result", "error"])]
    pub kind: Option<String>,
    /// Only frames in this direction: `in` from the station, `out` to it
    #[arg(long, value_name = "DIRECTION", value_parser = ["in", "out"])]
    pub direction: Option<String>,
}

/// Filters of the transaction routes.
#[derive(Debug, Args)]
pub struct TransactionFilter {
    /// Only the transactions of this station
    #[arg(long, value_name = "ID", value_parser = non_empty)]
    pub station: Option<String>,
    /// Only active or only stopped transactions
    #[arg(long, value_name = "STATE", value_parser = ["active", "stopped"])]
    pub state: Option<String>,
    /// Only transactions that started at or after this RFC 3339 time
    #[arg(long, value_name = "TIME", value_parser = parse_since_time)]
    pub since: Option<String>,
    /// At most this many transactions, 0 to 10000
    #[arg(long, value_name = "COUNT", value_parser = clap::value_parser!(u32).range(0..=10_000))]
    pub limit: Option<u32>,
}

/// A request payload: inline JSON or a file.
#[derive(Debug, Args)]
pub struct PayloadArgs {
    /// JSON payload (visible in `ps`: use --payload-file for secrets)
    #[arg(long, value_name = "JSON", conflicts_with = "payload_file")]
    pub payload: Option<String>,
    /// File holding the JSON payload; `-` reads standard input
    #[arg(long, value_name = "FILE", conflicts_with = "payload")]
    pub payload_file: Option<std::path::PathBuf>,
}

/// Largest payload accepted (the API refuses request bodies above 1 MiB).
const MAX_PAYLOAD: u64 = 1024 * 1024;

impl PayloadArgs {
    /// The payload as JSON, `None` when neither flag is given.
    pub fn read(&self) -> Result<Option<Value>, String> {
        use std::io::Read as _;
        let text = match (&self.payload, &self.payload_file) {
            (Some(text), _) => text.clone(),
            (None, Some(path)) => {
                let mut bytes = Vec::new();
                let read = if path.as_os_str() == "-" {
                    std::io::stdin()
                        .lock()
                        .take(MAX_PAYLOAD + 1)
                        .read_to_end(&mut bytes)
                } else {
                    std::fs::File::open(path)
                        .and_then(|file| file.take(MAX_PAYLOAD + 1).read_to_end(&mut bytes))
                };
                read.map_err(|e| format!("cannot read the payload from {}: {e}", path.display()))?;
                String::from_utf8(bytes)
                    .map_err(|_| format!("{} is not valid UTF-8", path.display()))?
            }
            (None, None) => return Ok(None),
        };
        if text.len() as u64 > MAX_PAYLOAD {
            return Err("the payload is larger than 1 MiB".to_owned());
        }
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("the payload is not valid JSON: {e}"))
    }
}

pub fn health() -> Spec {
    Spec::json("GET", "/v1/health", "/v1/health")
}

pub fn index() -> Spec {
    Spec::json("GET", "/v1", "/v1")
}

pub fn stations() -> Spec {
    Spec::json("GET", "/v1/stations", "/v1/stations")
}

pub fn station(id: &str) -> Spec {
    Spec::json(
        "GET",
        "/v1/stations/{id}",
        &format!("/v1/stations/{}", enc(id)),
    )
}

pub fn forget(id: &str) -> Spec {
    Spec::json(
        "DELETE",
        "/v1/stations/{id}",
        &format!("/v1/stations/{}", enc(id)),
    )
}

pub fn call(id: &str, action: &str, payload: Option<Value>, timeout_ms: Option<u32>) -> Spec {
    let mut body = Map::new();
    body.insert("action".to_owned(), json!(action));
    if let Some(payload) = payload {
        body.insert("payload".to_owned(), payload);
    }
    if let Some(timeout_ms) = timeout_ms {
        body.insert("timeout_ms".to_owned(), json!(timeout_ms));
    }
    Spec::json(
        "POST",
        "/v1/stations/{id}/calls",
        &format!("/v1/stations/{}/calls", enc(id)),
    )
    .with_body(Value::Object(body))
}

pub fn bulk(
    pattern: &str,
    action: &str,
    payload: Option<Value>,
    protocol: Option<&str>,
    timeout_ms: Option<u32>,
) -> Spec {
    let mut body = Map::new();
    body.insert("station".to_owned(), json!(pattern));
    body.insert("action".to_owned(), json!(action));
    if let Some(payload) = payload {
        body.insert("payload".to_owned(), payload);
    }
    if let Some(protocol) = protocol {
        body.insert("protocol".to_owned(), json!(protocol));
    }
    if let Some(timeout_ms) = timeout_ms {
        body.insert("timeout_ms".to_owned(), json!(timeout_ms));
    }
    Spec::json("POST", "/v1/calls", "/v1/calls").with_body(Value::Object(body))
}

pub fn history(id: &str, filter: &HistoryFilter, format: Option<&str>) -> Spec {
    let path = with_query(
        &format!("/v1/stations/{}/history", enc(id)),
        &[
            ("limit", filter.limit.map(|n| n.to_string())),
            ("since", filter.since.clone()),
            ("action", filter.action.clone()),
            ("kind", filter.kind.clone()),
            ("direction", filter.direction.clone()),
            ("format", format.map(str::to_owned)),
        ],
    );
    let kind = if format.is_some_and(|f| f != "json") {
        Kind::Text
    } else {
        Kind::Json
    };
    Spec::new("GET", "/v1/stations/{id}/history", path, kind)
}

pub fn transactions(filter: &TransactionFilter, format: Option<&str>) -> Spec {
    let path = with_query(
        "/v1/transactions",
        &[
            ("station", filter.station.clone()),
            ("state", filter.state.clone()),
            ("since", filter.since.clone()),
            ("limit", filter.limit.map(|n| n.to_string())),
            ("format", format.map(str::to_owned)),
        ],
    );
    let kind = if format.is_some_and(|f| f != "json") {
        Kind::Text
    } else {
        Kind::Json
    };
    Spec::new("GET", "/v1/transactions", path, kind)
}

pub fn metrics() -> Spec {
    Spec::new("GET", "/v1/metrics", "/v1/metrics".to_owned(), Kind::Text)
}

pub fn rules_list(station: Option<&str>) -> Spec {
    match station {
        Some(id) => Spec::json(
            "GET",
            "/v1/stations/{id}/rules",
            &format!("/v1/stations/{}/rules", enc(id)),
        ),
        None => Spec::json("GET", "/v1/rules", "/v1/rules"),
    }
}

pub fn rule_show(station: &str, action: &str) -> Spec {
    Spec::json(
        "GET",
        "/v1/stations/{id}/rules/{action}",
        &format!("/v1/stations/{}/rules/{}", enc(station), enc(action)),
    )
}

pub fn rule_set(
    action: &str,
    station: Option<&str>,
    protocol: Option<&str>,
    payload: Value,
) -> Spec {
    let protocol = protocol.map(str::to_owned);
    let (route, base) = match station {
        Some(id) => (
            "/v1/stations/{id}/rules/{action}",
            format!("/v1/stations/{}/rules/{}", enc(id), enc(action)),
        ),
        None => ("/v1/rules/{action}", format!("/v1/rules/{}", enc(action))),
    };
    Spec::json("PUT", route, &with_query(&base, &[("protocol", protocol)])).with_body(payload)
}

/// A station rule is removed without a protocol; a global one with an optional protocol.
pub fn rule_clear(action: &str, station: Option<&str>, protocol: Option<&str>) -> Spec {
    match station {
        Some(id) => Spec::json(
            "DELETE",
            "/v1/stations/{id}/rules/{action}",
            &format!("/v1/stations/{}/rules/{}", enc(id), enc(action)),
        ),
        None => Spec::json(
            "DELETE",
            "/v1/rules/{action}",
            &with_query(
                &format!("/v1/rules/{}", enc(action)),
                &[("protocol", protocol.map(str::to_owned))],
            ),
        ),
    }
}

pub fn faults_list() -> Spec {
    Spec::json("GET", "/v1/faults", "/v1/faults")
}

/// The fields of a new fault.
#[derive(Debug)]
pub struct FaultInput<'a> {
    pub action: &'a str,
    pub kind: &'a str,
    pub payload: Option<Value>,
    pub times: Option<u32>,
    pub station: Option<&'a str>,
    pub protocol: Option<&'a str>,
}

pub fn fault_add(input: FaultInput<'_>) -> Spec {
    let mut body = Map::new();
    body.insert("action".to_owned(), json!(input.action));
    body.insert("kind".to_owned(), json!(input.kind));
    if let Some(payload) = input.payload {
        body.insert("payload".to_owned(), payload);
    }
    if let Some(times) = input.times {
        body.insert("times".to_owned(), json!(times));
    }
    if let Some(station) = input.station {
        body.insert("station".to_owned(), json!(station));
    }
    if let Some(protocol) = input.protocol {
        body.insert("protocol".to_owned(), json!(protocol));
    }
    Spec::json("POST", "/v1/faults", "/v1/faults").with_body(Value::Object(body))
}

pub fn fault_remove(id: &str) -> Spec {
    Spec::json(
        "DELETE",
        "/v1/faults/{id}",
        &format!("/v1/faults/{}", enc(id)),
    )
}

pub fn faults_clear() -> Spec {
    Spec::json("DELETE", "/v1/faults", "/v1/faults")
}

/// The event stream, optionally for one station.
pub fn events(station: Option<&str>) -> String {
    with_query("/v1/events", &[("station", station.map(str::to_owned))])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> Value {
        serde_json::from_str(include_str!("../../../../docs/kirby-csms-openapi.json")).unwrap()
    }

    fn valid(document: &Value, schema: &str, body: &Value) {
        let wrapper = json!({
            "$ref": format!("#/components/schemas/{schema}"),
            "components": document["components"],
        });
        let validator = jsonschema::validator_for(&wrapper).unwrap();
        assert!(validator.is_valid(body), "{schema} rejects {body}");
    }

    fn history_filter() -> HistoryFilter {
        HistoryFilter {
            limit: Some(5),
            since: Some("c42".to_owned()),
            action: Some("Boot Notification+".to_owned()),
            kind: Some("call".to_owned()),
            direction: Some("in".to_owned()),
        }
    }

    fn fault(kind: &str, extra: bool) -> Spec {
        fault_add(FaultInput {
            action: "Heartbeat",
            kind,
            payload: (kind == "invalid_payload").then(|| json!({"currentTime": 1})),
            times: extra.then_some(3),
            station: extra.then_some("CP-1"),
            protocol: extra.then_some("ocpp1.6"),
        })
    }

    #[test]
    fn every_request_names_a_route_and_method_of_the_openapi_document() {
        let document = document();
        let specs = [
            health(),
            index(),
            stations(),
            station("CP-1"),
            forget("CP-1"),
            call("CP-1", "Reset", Some(json!({"type": "Soft"})), Some(5000)),
            bulk("CP-*", "Reset", None, Some("ocpp1.6"), None),
            history("CP-1", &history_filter(), Some("csv")),
            transactions(
                &TransactionFilter {
                    station: Some("CP-1".to_owned()),
                    state: Some("active".to_owned()),
                    since: Some("2030-01-01T10:00:00+02:00".to_owned()),
                    limit: Some(7),
                },
                None,
            ),
            metrics(),
            rules_list(None),
            rules_list(Some("CP-1")),
            rule_show("CP-1", "Heartbeat"),
            rule_set("Heartbeat", None, Some("ocpp1.6"), json!({})),
            rule_set("Heartbeat", Some("CP-1"), None, json!({})),
            rule_clear("Heartbeat", None, Some("ocpp1.6")),
            rule_clear("Heartbeat", Some("CP-1"), None),
            faults_list(),
            fault("call_error", true),
            fault_remove("f1"),
            faults_clear(),
        ];
        for spec in specs {
            let operation = &document["paths"][spec.route][spec.method.to_lowercase()];
            assert!(operation.is_object(), "{} {}", spec.method, spec.route);
            assert!(spec.path.starts_with('/'));
            assert!(!spec.path.contains(' '), "{}", spec.path);
        }
        let events = events(Some("CP 1"));
        assert_eq!(events, "/v1/events?station=CP%201");
        assert!(document["paths"]["/v1/events"]["get"].is_object());
    }

    #[test]
    fn request_bodies_validate_against_the_schemas() {
        let document = document();
        for spec in [
            call("CP-1", "Reset", None, None),
            call(
                "CP-1",
                "Reset",
                Some(json!({"type": "Soft"})),
                Some(86_400_000),
            ),
        ] {
            valid(&document, "CallRequest", spec.body.as_ref().unwrap());
        }
        for spec in [
            bulk("CP-*", "Reset", None, None, None),
            bulk(
                "?",
                "Reset",
                Some(json!({"a": 1})),
                Some("ocpp1.6"),
                Some(1),
            ),
        ] {
            valid(&document, "BulkCall", spec.body.as_ref().unwrap());
        }
        for spec in [
            fault("call_error", false),
            fault("silence", true),
            fault("close", true),
            fault("invalid_payload", true),
        ] {
            valid(&document, "FaultRequest", spec.body.as_ref().unwrap());
        }
        // Absent options are not sent at all.
        let body = fault("silence", false).body.unwrap();
        assert_eq!(body.as_object().unwrap().len(), 2);
        let body = bulk("*", "Reset", None, None, None).body.unwrap();
        assert_eq!(body.as_object().unwrap().len(), 2);
    }

    #[test]
    fn path_segments_and_query_values_are_encoded() {
        assert_eq!(station("CP 1+я/x").path, "/v1/stations/CP%201%2B%D1%8F%2Fx");
        let spec = history("CP-1", &history_filter(), None);
        assert_eq!(
            spec.path,
            "/v1/stations/CP-1/history?limit=5&since=c42&action=Boot%20Notification%2B&kind=call&direction=in"
        );
        let spec = transactions(
            &TransactionFilter {
                station: None,
                state: None,
                since: Some("2030-01-01T10:00:00+02:00".to_owned()),
                limit: None,
            },
            Some("csv"),
        );
        assert_eq!(
            spec.path,
            "/v1/transactions?since=2030-01-01T10%3A00%3A00%2B02%3A00&format=csv"
        );
        assert_eq!(spec.kind, Kind::Text);
        assert_eq!(
            rule_clear("Heartbeat", Some("CP-1"), Some("ocpp1.6")).path,
            "/v1/stations/CP-1/rules/Heartbeat",
            "a station rule is removed without a query"
        );
    }

    #[test]
    fn cursors_and_times_are_checked() {
        assert!(parse_cursor("c0").is_ok());
        for bad in ["", "c", "42", "cx1", "c-1", "c1 "] {
            assert!(parse_cursor(bad).is_err(), "{bad}");
        }
        assert!(parse_since_time("2030-01-01T10:00:00Z").is_ok());
        assert!(parse_since_time("yesterday").is_err());
        assert_eq!(parse_millis("1").unwrap(), 1);
        assert!(parse_millis("0").is_err());
        assert!(parse_millis("86400001").is_err());
    }
}
