//! The OCPP protocol versions and their wire identifiers.

use std::fmt;

use serde::{Deserialize, Serialize};

/// An OCPP version that travels over OCPP-J (JSON over WebSocket).
///
/// Each version has a WebSocket subprotocol token (`ocpp1.6`) and a bare version string
/// (`1.6`) that report, scenario and recording files carry. All three versions are
/// implemented; [`Protocol::V21`] runs the 2.0.1 behavior with the 2.1 schemas, action
/// catalog and message types (see [`SchemaRegistry`](crate::ocpp::SchemaRegistry) and
/// [`registry`](crate::ocpp::v2::registry)).
/// Serialized as the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Protocol {
    /// OCPP 1.6J.
    #[serde(rename = "ocpp1.6")]
    V16,
    /// OCPP 2.0.1.
    #[serde(rename = "ocpp2.0.1")]
    V201,
    /// OCPP 2.1.
    #[serde(rename = "ocpp2.1")]
    V21,
}

impl Protocol {
    /// Every version, oldest first.
    pub const ALL: [Self; 3] = [Self::V16, Self::V201, Self::V21];

    /// The WebSocket subprotocol name: `ocpp1.6`, `ocpp2.0.1` or `ocpp2.1`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::V16 => "ocpp1.6",
            Self::V201 => "ocpp2.0.1",
            Self::V21 => "ocpp2.1",
        }
    }

    /// The bare version string used in report, scenario and recording files: `1.6`,
    /// `2.0.1` or `2.1`.
    #[must_use]
    pub const fn ocpp_version(self) -> &'static str {
        match self {
            Self::V16 => "1.6",
            Self::V201 => "2.0.1",
            Self::V21 => "2.1",
        }
    }

    /// The version whose [`token`](Self::token) is exactly `token` (case-sensitive, no
    /// trimming).
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.token() == token)
    }

    /// Whether this build can act as a charge station for the version.
    #[must_use]
    pub const fn is_station_implemented(self) -> bool {
        matches!(self, Self::V16 | Self::V201 | Self::V21)
    }

    /// Whether this build can act as a central system for the version.
    #[must_use]
    pub const fn is_csms_implemented(self) -> bool {
        matches!(self, Self::V16 | Self::V201 | Self::V21)
    }
}

impl fmt::Display for Protocol {
    /// The subprotocol token.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn tokens_and_versions_are_the_wire_strings() {
        assert_eq!(Protocol::V16.token(), "ocpp1.6");
        assert_eq!(Protocol::V201.token(), "ocpp2.0.1");
        assert_eq!(Protocol::V21.token(), "ocpp2.1");
        assert_eq!(Protocol::V16.ocpp_version(), "1.6");
        assert_eq!(Protocol::V201.ocpp_version(), "2.0.1");
        assert_eq!(Protocol::V21.ocpp_version(), "2.1");
    }

    #[test]
    fn a_token_is_the_version_behind_the_ocpp_prefix() {
        for protocol in Protocol::ALL {
            assert_eq!(
                protocol.token().strip_prefix("ocpp"),
                Some(protocol.ocpp_version())
            );
        }
    }

    #[test]
    fn tokens_round_trip() {
        for protocol in Protocol::ALL {
            assert_eq!(Protocol::from_token(protocol.token()), Some(protocol));
            assert_eq!(protocol.to_string(), protocol.token());
        }
        let unique: HashSet<_> = Protocol::ALL.iter().map(|p| p.token()).collect();
        assert_eq!(unique.len(), Protocol::ALL.len());
    }

    #[test]
    fn from_token_is_an_exact_match() {
        for text in [
            "",
            "1.6",
            "OCPP1.6",
            "ocpp1.6 ",
            " ocpp1.6",
            "ocpp2",
            "ocpp2.0",
            "ocpp2.1.0",
            "ocpp1.6,ocpp2.0.1",
        ] {
            assert_eq!(Protocol::from_token(text), None, "{text:?}");
        }
    }

    #[test]
    fn serde_uses_the_token() {
        for protocol in Protocol::ALL {
            let json = serde_json::to_value(protocol).unwrap();
            assert_eq!(json, serde_json::Value::from(protocol.token()));
            assert_eq!(serde_json::from_value::<Protocol>(json).unwrap(), protocol);
        }
        assert!(serde_json::from_value::<Protocol>(serde_json::json!("1.6")).is_err());
    }

    #[test]
    fn station_and_csms_implement_every_version() {
        for protocol in [Protocol::V16, Protocol::V201, Protocol::V21] {
            assert!(protocol.is_station_implemented());
            assert!(protocol.is_csms_implemented());
        }
    }
}
