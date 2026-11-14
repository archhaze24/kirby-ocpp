//! The OCPP 2.0.1 payload layer.
//!
//! [`types`] holds the shared structures and enumerations, [`messages`] the
//! typed requests, responses and action markers. Both are independent of the
//! OCPP 1.6 definitions in [`crate::ocpp::types`] and [`crate::ocpp::messages`],
//! even where names overlap.
//!
//! The markers implement the same [`Action`](crate::ocpp::action::Action),
//! [`OutboundAction`](crate::ocpp::action::OutboundAction) and
//! [`InboundAction`](crate::ocpp::action::InboundAction) traits as the 1.6
//! ones. Direction is relative to the charge station, also when the CSMS
//! sends the message.
//!
//! The 2.0.1 JSON schemas, not these structs, define validity. A payload is
//! validated as raw JSON first and decoded afterwards, so decoding is
//! deliberately lenient: unknown fields outside `customData` are not rejected
//! here. Actions without a typed payload are carried as raw JSON.

pub mod messages;
pub mod raw;
pub mod registry;
pub mod types;

pub use raw::{NotAnObject, RawAction, RawValidationError};
pub use registry::{ActionDirection, ActionInfo};

#[cfg(test)]
pub(crate) mod test_support {
    //! Fixture checks shared by the payload tests.

    use std::collections::BTreeSet;

    use jsonschema::Validator;
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use serde_json::Value;

    use super::types::WireEnum;
    use crate::ocpp::action::Action;

    fn load(file: &str) -> Value {
        let path = format!("{}/schemas/ocpp201/{file}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn validator(file: &str) -> Validator {
        jsonschema::draft6::options()
            .should_validate_formats(true)
            .build(&load(file))
            .unwrap()
    }

    /// Asserts that `fixture` validates against `file`, decodes into `T` and
    /// encodes back to exactly the same JSON.
    pub(crate) fn check_roundtrip<T>(file: &str, fixture: &Value)
    where
        T: Serialize + DeserializeOwned,
    {
        let errors: Vec<String> = validator(file)
            .iter_errors(fixture)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{file}: fixture invalid: {errors:?}");
        let typed: T = serde_json::from_value(fixture.clone())
            .unwrap_or_else(|e| panic!("{file}: decode failed: {e}"));
        let back = serde_json::to_value(&typed).unwrap();
        assert_eq!(&back, fixture, "{file}: roundtrip changed the payload");
        let errors: Vec<String> = validator(file)
            .iter_errors(&back)
            .map(|e| e.to_string())
            .collect();
        assert!(
            errors.is_empty(),
            "{file}: encoded payload invalid: {errors:?}"
        );
    }

    /// Roundtrips one request and one response fixture of action `A`.
    pub(crate) fn check_action<A: Action>(request: &Value, response: &Value) {
        check_roundtrip::<A::Req>(&format!("{}Request.json", A::NAME), request);
        check_roundtrip::<A::Res>(&format!("{}Response.json", A::NAME), response);
    }

    /// Asserts that `E` lists exactly the values of definition `def` in `file`.
    pub(crate) fn check_enum<E: WireEnum + Serialize>(file: &str, def: &str) {
        let schema = load(file);
        let expected: BTreeSet<String> = schema["definitions"][def]["enum"]
            .as_array()
            .unwrap_or_else(|| panic!("{file}: no enum {def}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        let actual: BTreeSet<String> = E::ALL.iter().map(|v| v.as_str().to_owned()).collect();
        assert_eq!(actual, expected, "{file}#{def}");
        assert_eq!(
            E::ALL.len(),
            expected.len(),
            "{file}#{def}: duplicate variant"
        );
        for value in E::ALL {
            assert_eq!(
                serde_json::to_value(value).unwrap(),
                Value::String(value.as_str().to_owned()),
                "{file}#{def}: serde name differs from as_str"
            );
        }
    }
}
