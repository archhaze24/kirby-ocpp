use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ocpp::action::{Action, OutboundAction};

/// `Heartbeat` (charge point -> central system).
#[derive(Debug, Clone, Copy)]
pub struct Heartbeat;

/// `Heartbeat.req`: no fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRequest {}

/// `Heartbeat.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatResponse {
    /// The central system's current time.
    pub current_time: DateTime<Utc>,
}

impl Action for Heartbeat {
    const NAME: &'static str = "Heartbeat";
    type Req = HeartbeatRequest;
    type Res = HeartbeatResponse;
}

impl OutboundAction for Heartbeat {}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;

    #[test]
    fn request_is_an_empty_object() {
        assert_eq!(
            serde_json::to_value(HeartbeatRequest {}).unwrap(),
            json!({})
        );
        assert_eq!(
            serde_json::from_value::<HeartbeatRequest>(json!({})).unwrap(),
            HeartbeatRequest {}
        );
    }

    #[test]
    fn response_uses_camel_case_and_rfc3339() {
        let response = HeartbeatResponse {
            current_time: Utc.with_ymd_and_hms(2026, 5, 6, 7, 8, 9).unwrap(),
        };
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value, json!({"currentTime": "2026-05-06T07:08:09Z"}));
        assert_eq!(
            serde_json::from_value::<HeartbeatResponse>(value).unwrap(),
            response
        );
    }

    #[test]
    fn response_accepts_offset_and_millisecond_timestamps() {
        let parsed: HeartbeatResponse =
            serde_json::from_value(json!({"currentTime": "2026-05-06T09:08:09.123+02:00"}))
                .unwrap();
        assert_eq!(
            parsed.current_time,
            Utc.with_ymd_and_hms(2026, 5, 6, 7, 8, 9).unwrap()
                + chrono::Duration::milliseconds(123)
        );
    }

    #[test]
    fn response_without_current_time_does_not_parse() {
        assert!(serde_json::from_value::<HeartbeatResponse>(json!({})).is_err());
    }

    #[test]
    fn action_name() {
        assert_eq!(Heartbeat::NAME, "Heartbeat");
    }
}
