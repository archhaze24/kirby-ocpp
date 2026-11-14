//! The JSON-schema registries the engine validates with, one per protocol.

use std::sync::Arc;

use kirby_ocpp_core::ocpp::{ErrorCode, Protocol, SchemaRegistry};

/// The registries of every protocol this build has schemas for, loaded when the server starts.
///
/// A protocol without a registry (an unknown one, or one the build embeds no schemas for)
/// is treated as "no schema known": nothing is validated for it.
pub(crate) struct EngineSchemas {
    registries: Vec<Arc<SchemaRegistry>>,
}

impl EngineSchemas {
    /// Loads the shared registry of each protocol that has one. A registry that fails to
    /// compile is a build bug; validation is skipped for it instead of failing the server.
    pub(crate) fn load() -> Self {
        let registries = Protocol::ALL
            .into_iter()
            .filter_map(|protocol| SchemaRegistry::for_protocol(protocol).ok())
            .collect();
        Self { registries }
    }

    /// The registry of `protocol`, if the build has one.
    pub(crate) fn schemas_for(&self, protocol: Protocol) -> Option<&SchemaRegistry> {
        self.registries
            .iter()
            .find(|registry| registry.protocol() == protocol)
            .map(AsRef::as_ref)
    }

    /// Checks the request of an inbound CALL. `None` when the action is unknown to the
    /// schemas of `protocol` (or there are none) or the payload is valid; otherwise the
    /// CALLERROR code in the spelling of `protocol` and the violations.
    pub(crate) fn inbound_violation(
        &self,
        protocol: Protocol,
        action: &str,
        payload: &serde_json::Value,
    ) -> Option<(ErrorCode, String)> {
        let schemas = self.schemas_for(protocol)?;
        if !schemas.knows_action(action) {
            return None;
        }
        let violation = schemas.validate_request(action, payload).err()?;
        Some((
            violation.error_code_for(protocol),
            violation.errors.join("; "),
        ))
    }
}

/// The protocol of a session from its negotiated subprotocol. A session that negotiated
/// nothing, or a name this build does not know, is treated as OCPP 1.6.
pub(crate) fn session_protocol(subprotocol: Option<&str>) -> Protocol {
    subprotocol
        .and_then(Protocol::from_token)
        .unwrap_or(Protocol::V16)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn only_protocols_with_schemas_have_a_registry() {
        let schemas = EngineSchemas::load();
        let v16 = schemas.schemas_for(Protocol::V16).expect("1.6 schemas");
        assert_eq!(v16.protocol(), Protocol::V16);
        for protocol in Protocol::ALL {
            assert_eq!(
                schemas.schemas_for(protocol).is_some(),
                SchemaRegistry::for_protocol(protocol).is_ok()
            );
        }
    }

    #[test]
    fn a_protocol_without_schemas_validates_nothing() {
        let schemas = EngineSchemas::load();
        // A payload that breaks the 1.6 schema of BootNotification.
        let bad = json!({});
        assert!(
            schemas
                .inbound_violation(Protocol::V16, "BootNotification", &bad)
                .is_some()
        );
        if schemas.schemas_for(Protocol::V201).is_none() {
            assert!(
                schemas
                    .inbound_violation(Protocol::V201, "BootNotification", &bad)
                    .is_none()
            );
        }
    }

    #[test]
    fn a_2_0_1_session_request_is_validated_against_the_2_0_1_schemas() {
        let schemas = EngineSchemas::load();
        let (code, _) = schemas
            .inbound_violation(
                Protocol::V201,
                "BootNotification",
                &json!({"chargePointVendor": "v", "chargePointModel": "m"}),
            )
            .expect("a 1.6 boot payload violates the 2.0.1 schema");
        assert_eq!(code.as_str(), "OccurrenceConstraintViolation");
        // Unknown names are left to the dispatcher.
        assert!(
            schemas
                .inbound_violation(Protocol::V201, "RemoteStartTransaction", &json!({}))
                .is_none()
        );
        assert!(
            schemas
                .inbound_violation(
                    Protocol::V201,
                    "BootNotification",
                    &json!({"reason": "PowerUp", "chargingStation": {"model": "m", "vendorName": "v"}}),
                )
                .is_none()
        );
    }

    #[test]
    fn the_subprotocol_picks_the_session_protocol() {
        assert_eq!(session_protocol(None), Protocol::V16);
        assert_eq!(session_protocol(Some("ocpp1.6")), Protocol::V16);
        assert_eq!(session_protocol(Some("ocpp2.0.1")), Protocol::V201);
        assert_eq!(session_protocol(Some("ocpp2.1")), Protocol::V21);
        assert_eq!(session_protocol(Some("mqtt")), Protocol::V16);
    }
}
