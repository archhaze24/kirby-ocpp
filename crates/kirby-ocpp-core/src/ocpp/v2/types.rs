//! Shared OCPP 2.0.1 payload structures and enumerations.
//!
//! Conventions:
//!
//! * Names follow the schema definitions without the `Type` / `EnumType`
//!   suffix. The enums are independent of the OCPP 1.6 ones in
//!   [`crate::ocpp::types`] even where names overlap.
//! * Every struct keeps its optional `customData` ([`CustomData`]).
//! * Omitted optionals stay omitted on serialization; schema defaults
//!   (`offline`, `persistent`, `unit`, ...) are never materialized.
//! * Optional arrays are omitted when `None` or empty, because the schemas
//!   require at least one element when the property is present.
//! * Unknown fields outside `customData` are rejected by schema validation
//!   before decoding, so the structs do not use `deny_unknown_fields`.
//! * Enums have no catch-all variant: schema validation precedes decoding.
//! * Timestamps deserialize from any RFC 3339 value and serialize with
//!   millisecond precision and a `Z` suffix.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

/// A wire enum with a fixed set of string values.
pub trait WireEnum: Sized + 'static {
    /// Every value, in schema order.
    const ALL: &'static [Self];

    /// The exact wire string.
    fn as_str(&self) -> &'static str;
}

macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $($(#[$vmeta:meta])* $variant:ident = $wire:literal),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$vmeta])* #[serde(rename = $wire)] $variant,)+
        }

        impl $crate::ocpp::v2::types::WireEnum for $name {
            const ALL: &'static [Self] = &[$(Self::$variant),+];

            fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)+
                }
            }
        }
    };
}
pub(crate) use wire_enum;

/// `serde` helpers shared by the payload structs.
pub(crate) mod helpers {
    use super::{DateTime, Deserialize, SecondsFormat, Utc, Value};
    use serde::{Deserializer, Serializer};

    /// `skip_serializing_if` for optional arrays: absent or empty.
    #[allow(clippy::ref_option)]
    pub fn omit_empty<T>(value: &Option<Vec<T>>) -> bool {
        value.as_ref().is_none_or(Vec::is_empty)
    }

    /// Required timestamp: RFC 3339 in, milliseconds out.
    pub mod ts {
        use super::{DateTime, Deserialize, Deserializer, SecondsFormat, Serializer, Utc};

        pub fn serialize<S: Serializer>(value: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_str(&value.to_rfc3339_opts(SecondsFormat::Millis, true))
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
            DateTime::<Utc>::deserialize(d)
        }
    }

    /// Optional timestamp; pair with `#[serde(default)]` and
    /// `skip_serializing_if = "Option::is_none"`.
    pub mod ts_opt {
        use super::{DateTime, Deserialize, Deserializer, SecondsFormat, Serializer, Utc};

        #[allow(clippy::ref_option)]
        pub fn serialize<S: Serializer>(
            value: &Option<DateTime<Utc>>,
            s: S,
        ) -> Result<S::Ok, S::Error> {
            match value {
                Some(t) => s.serialize_str(&t.to_rfc3339_opts(SecondsFormat::Millis, true)),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Option<DateTime<Utc>>, D::Error> {
            Option::<DateTime<Utc>>::deserialize(d)
        }
    }

    /// `AnyType` property: `None` is absent, `Some(Value::Null)` is an
    /// explicit JSON `null`. Pair with `#[serde(default)]` and
    /// `skip_serializing_if = "Option::is_none"`.
    pub mod any_type {
        use super::{Deserialize, Deserializer, Value};

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
            Value::deserialize(d).map(Some)
        }
    }
}

/// Vendor extension carried by the optional `customData` property.
///
/// Only `vendorId` is defined; any other property is kept as-is in
/// [`extra`](Self::extra), including nested JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomData {
    pub vendor_id: String,
    /// Arbitrary additional properties.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl CustomData {
    /// Custom data with only the vendor id.
    pub fn new(vendor_id: impl Into<String>) -> Self {
        Self {
            vendor_id: vendor_id.into(),
            extra: Map::new(),
        }
    }
}

macro_rules! payload {
    (
        $(#[$meta:meta])*
        $name:ident { $($body:tt)* }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name {
            $($body)*
            /// Vendor extension.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub custom_data: Option<CustomData>,
        }
    };
}
pub(crate) use payload;

wire_enum! {
    /// Kind of an [`IdToken`] (`IdTokenEnumType`).
    IdTokenType {
        Central = "Central",
        Emaid = "eMAID",
        Iso14443 = "ISO14443",
        Iso15693 = "ISO15693",
        KeyCode = "KeyCode",
        Local = "Local",
        MacAddress = "MacAddress",
        NoAuthorization = "NoAuthorization",
    }
}

wire_enum! {
    /// Authorization result (`AuthorizationStatusEnumType`).
    AuthorizationStatus {
        Accepted = "Accepted",
        Blocked = "Blocked",
        ConcurrentTx = "ConcurrentTx",
        Expired = "Expired",
        Invalid = "Invalid",
        NoCredit = "NoCredit",
        NotAllowedTypeEvse = "NotAllowedTypeEVSE",
        NotAtThisLocation = "NotAtThisLocation",
        NotAtThisTime = "NotAtThisTime",
        Unknown = "Unknown",
    }
}

wire_enum! {
    /// Format of a [`MessageContent`] (`MessageFormatEnumType`).
    MessageFormat {
        Ascii = "ASCII",
        Html = "HTML",
        Uri = "URI",
        Utf8 = "UTF8",
    }
}

wire_enum! {
    /// Hash algorithm of [`OcspRequestData`] (`HashAlgorithmEnumType`).
    HashAlgorithm {
        Sha256 = "SHA256",
        Sha384 = "SHA384",
        Sha512 = "SHA512",
    }
}

wire_enum! {
    /// Charging state of a transaction (`ChargingStateEnumType`).
    ChargingState {
        Charging = "Charging",
        EvConnected = "EVConnected",
        SuspendedEv = "SuspendedEV",
        SuspendedEvse = "SuspendedEVSE",
        Idle = "Idle",
    }
}

wire_enum! {
    /// Why a transaction stopped (`ReasonEnumType`).
    StopReason {
        DeAuthorized = "DeAuthorized",
        EmergencyStop = "EmergencyStop",
        EnergyLimitReached = "EnergyLimitReached",
        EvDisconnected = "EVDisconnected",
        GroundFault = "GroundFault",
        ImmediateReset = "ImmediateReset",
        Local = "Local",
        LocalOutOfCredit = "LocalOutOfCredit",
        MasterPass = "MasterPass",
        Other = "Other",
        OvercurrentFault = "OvercurrentFault",
        PowerLoss = "PowerLoss",
        PowerQuality = "PowerQuality",
        Reboot = "Reboot",
        Remote = "Remote",
        SocLimitReached = "SOCLimitReached",
        StoppedByEv = "StoppedByEV",
        TimeLimitReached = "TimeLimitReached",
        Timeout = "Timeout",
    }
}

wire_enum! {
    /// Where a sample was taken (`LocationEnumType`).
    Location {
        Body = "Body",
        Cable = "Cable",
        Ev = "EV",
        Inlet = "Inlet",
        Outlet = "Outlet",
    }
}

wire_enum! {
    /// What a sample measures (`MeasurandEnumType`).
    Measurand {
        CurrentExport = "Current.Export",
        CurrentImport = "Current.Import",
        CurrentOffered = "Current.Offered",
        EnergyActiveExportRegister = "Energy.Active.Export.Register",
        EnergyActiveImportRegister = "Energy.Active.Import.Register",
        EnergyReactiveExportRegister = "Energy.Reactive.Export.Register",
        EnergyReactiveImportRegister = "Energy.Reactive.Import.Register",
        EnergyActiveExportInterval = "Energy.Active.Export.Interval",
        EnergyActiveImportInterval = "Energy.Active.Import.Interval",
        EnergyActiveNet = "Energy.Active.Net",
        EnergyReactiveExportInterval = "Energy.Reactive.Export.Interval",
        EnergyReactiveImportInterval = "Energy.Reactive.Import.Interval",
        EnergyReactiveNet = "Energy.Reactive.Net",
        EnergyApparentNet = "Energy.Apparent.Net",
        EnergyApparentImport = "Energy.Apparent.Import",
        EnergyApparentExport = "Energy.Apparent.Export",
        Frequency = "Frequency",
        PowerActiveExport = "Power.Active.Export",
        PowerActiveImport = "Power.Active.Import",
        PowerFactor = "Power.Factor",
        PowerOffered = "Power.Offered",
        PowerReactiveExport = "Power.Reactive.Export",
        PowerReactiveImport = "Power.Reactive.Import",
        Soc = "SoC",
        Voltage = "Voltage",
    }
}

wire_enum! {
    /// Phase a sample refers to (`PhaseEnumType`).
    Phase {
        L1 = "L1",
        L2 = "L2",
        L3 = "L3",
        N = "N",
        L1N = "L1-N",
        L2N = "L2-N",
        L3N = "L3-N",
        L1L2 = "L1-L2",
        L2L3 = "L2-L3",
        L3L1 = "L3-L1",
    }
}

wire_enum! {
    /// Why a sample was taken (`ReadingContextEnumType`).
    ReadingContext {
        InterruptionBegin = "Interruption.Begin",
        InterruptionEnd = "Interruption.End",
        Other = "Other",
        SampleClock = "Sample.Clock",
        SamplePeriodic = "Sample.Periodic",
        TransactionBegin = "Transaction.Begin",
        TransactionEnd = "Transaction.End",
        Trigger = "Trigger",
    }
}

wire_enum! {
    /// Attribute of a device model variable (`AttributeEnumType`).
    AttributeType {
        Actual = "Actual",
        Target = "Target",
        MinSet = "MinSet",
        MaxSet = "MaxSet",
    }
}

payload! {
    /// Charging station identity (`ChargingStationType`).
    ChargingStation {
        /// Max 25 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub serial_number: Option<String>,
        /// Max 20 characters.
        pub model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub modem: Option<Modem>,
        /// Max 50 characters.
        pub vendor_name: String,
        /// Max 50 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub firmware_version: Option<String>,
    }
}

payload! {
    /// Cellular modem identity (`ModemType`).
    Modem {
        /// Max 20 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub iccid: Option<String>,
        /// Max 20 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub imsi: Option<String>,
    }
}

payload! {
    /// EVSE, optionally narrowed to one connector (`EVSEType`).
    Evse {
        /// EVSE number; 0 addresses the whole station where a message allows it.
        pub id: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub connector_id: Option<i32>,
    }
}

payload! {
    /// Extra token identifier (`AdditionalInfoType`).
    AdditionalInfo {
        /// Max 36 characters.
        pub additional_id_token: String,
        /// Max 50 characters; names the kind of `additional_id_token`.
        #[serde(rename = "type")]
        pub kind: String,
    }
}

payload! {
    /// Authorization token (`IdTokenType`).
    IdToken {
        /// Absent or non-empty.
        #[serde(default, skip_serializing_if = "helpers::omit_empty")]
        pub additional_info: Option<Vec<AdditionalInfo>>,
        /// Max 36 characters.
        pub id_token: String,
        #[serde(rename = "type")]
        pub kind: IdTokenType,
    }
}

payload! {
    /// Text shown to the driver (`MessageContentType`).
    MessageContent {
        pub format: MessageFormat,
        /// Max 8 characters (RFC 5646).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub language: Option<String>,
        /// Max 512 characters.
        pub content: String,
    }
}

payload! {
    /// Authorization details for a token (`IdTokenInfoType`).
    IdTokenInfo {
        pub status: AuthorizationStatus,
        #[serde(default, with = "helpers::ts_opt", skip_serializing_if = "Option::is_none")]
        pub cache_expiry_date_time: Option<DateTime<Utc>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub charging_priority: Option<i32>,
        /// Max 8 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub language1: Option<String>,
        /// Absent or non-empty.
        #[serde(default, skip_serializing_if = "helpers::omit_empty")]
        pub evse_id: Option<Vec<i32>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub group_id_token: Option<IdToken>,
        /// Max 8 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub language2: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub personal_message: Option<MessageContent>,
    }
}

payload! {
    /// Machine-readable detail of a response status (`StatusInfoType`).
    StatusInfo {
        /// Max 20 characters.
        pub reason_code: String,
        /// Max 512 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub additional_info: Option<String>,
    }
}

payload! {
    /// Certificate hash data for an OCSP request (`OCSPRequestDataType`).
    OcspRequestData {
        pub hash_algorithm: HashAlgorithm,
        /// Max 128 characters.
        pub issuer_name_hash: String,
        /// Max 128 characters.
        pub issuer_key_hash: String,
        /// Max 40 characters.
        pub serial_number: String,
        /// Max 512 characters.
        #[serde(rename = "responderURL")]
        pub responder_url: String,
    }
}

payload! {
    /// Transaction details of a `TransactionEvent` (`TransactionType`).
    Transaction {
        /// Station-generated id, a string of up to 36 characters.
        pub transaction_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub charging_state: Option<ChargingState>,
        /// Seconds spent charging.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub time_spent_charging: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub stopped_reason: Option<StopReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub remote_start_id: Option<i32>,
    }
}

payload! {
    /// Signed meter reading (`SignedMeterValueType`).
    SignedMeterValue {
        /// Max 2500 characters.
        pub signed_meter_data: String,
        /// Max 50 characters.
        pub signing_method: String,
        /// Max 50 characters.
        pub encoding_method: String,
        /// Max 2500 characters.
        pub public_key: String,
    }
}

payload! {
    /// Unit of a sample (`UnitOfMeasureType`). The schema defaults (`Wh`,
    /// multiplier 0) apply when a field is absent and are not filled in here.
    UnitOfMeasure {
        /// Max 20 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub unit: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub multiplier: Option<i32>,
    }
}

payload! {
    /// One sample (`SampledValueType`).
    SampledValue {
        /// JSON number. `serde_json::Number` keeps integer and decimal
        /// spelling as received (decimals are limited to `f64` precision).
        pub value: Number,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub context: Option<ReadingContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub measurand: Option<Measurand>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub phase: Option<Phase>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub location: Option<Location>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub signed_meter_value: Option<SignedMeterValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub unit_of_measure: Option<UnitOfMeasure>,
    }
}

payload! {
    /// Samples taken at one time (`MeterValueType`).
    MeterValue {
        /// At least one sample.
        pub sampled_value: Vec<SampledValue>,
        #[serde(with = "helpers::ts")]
        pub timestamp: DateTime<Utc>,
    }
}

payload! {
    /// Device model component (`ComponentType`).
    Component {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evse: Option<Evse>,
        /// Max 50 characters.
        pub name: String,
        /// Max 50 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub instance: Option<String>,
    }
}

payload! {
    /// Device model variable (`VariableType`).
    Variable {
        /// Max 50 characters.
        pub name: String,
        /// Max 50 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub instance: Option<String>,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ocpp::v2::test_support::check_enum;

    #[test]
    fn enum_values_match_schema() {
        check_enum::<IdTokenType>("AuthorizeRequest.json", "IdTokenEnumType");
        check_enum::<AuthorizationStatus>("AuthorizeResponse.json", "AuthorizationStatusEnumType");
        check_enum::<MessageFormat>("AuthorizeResponse.json", "MessageFormatEnumType");
        check_enum::<HashAlgorithm>("AuthorizeRequest.json", "HashAlgorithmEnumType");
        check_enum::<ChargingState>("TransactionEventRequest.json", "ChargingStateEnumType");
        check_enum::<StopReason>("TransactionEventRequest.json", "ReasonEnumType");
        check_enum::<Location>("MeterValuesRequest.json", "LocationEnumType");
        check_enum::<Measurand>("MeterValuesRequest.json", "MeasurandEnumType");
        check_enum::<Phase>("MeterValuesRequest.json", "PhaseEnumType");
        check_enum::<ReadingContext>("MeterValuesRequest.json", "ReadingContextEnumType");
        check_enum::<AttributeType>("GetVariablesRequest.json", "AttributeEnumType");
    }

    #[test]
    fn custom_data_keeps_arbitrary_properties() {
        let wire = json!({
            "vendorId": "acme",
            "n": 1,
            "nested": {"list": [1, "two", null], "deep": {"k": false}}
        });
        let parsed: CustomData = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(parsed.vendor_id, "acme");
        assert_eq!(parsed.extra.len(), 2);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), wire);
    }

    #[test]
    fn custom_data_requires_vendor_id() {
        assert!(serde_json::from_value::<CustomData>(json!({"n": 1})).is_err());
    }

    #[test]
    fn meter_value_is_a_json_number() {
        let sample = |v: Value| {
            let parsed: SampledValue = serde_json::from_value(json!({"value": v})).unwrap();
            serde_json::to_value(parsed).unwrap()
        };
        assert_eq!(sample(json!(12345)), json!({"value": 12345}));
        assert_eq!(sample(json!(12.5)), json!({"value": 12.5}));
        assert_eq!(sample(json!(-3)), json!({"value": -3}));
        assert!(serde_json::from_value::<SampledValue>(json!({"value": "12"})).is_err());
    }

    #[test]
    fn timestamps_serialize_with_milliseconds() {
        let parsed: MeterValue = serde_json::from_value(json!({
            "sampledValue": [{"value": 1}],
            "timestamp": "2026-05-06T09:08:09.1239+02:00"
        }))
        .unwrap();
        let wire = serde_json::to_value(&parsed).unwrap();
        assert_eq!(wire["timestamp"], "2026-05-06T07:08:09.123Z");

        let whole: MeterValue = serde_json::from_value(json!({
            "sampledValue": [{"value": 1}],
            "timestamp": "2026-05-06T07:08:09Z"
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&whole).unwrap()["timestamp"],
            "2026-05-06T07:08:09.000Z"
        );
    }

    #[test]
    fn optional_timestamp_is_omitted_and_formatted() {
        let info = IdTokenInfo {
            status: AuthorizationStatus::Accepted,
            cache_expiry_date_time: None,
            charging_priority: None,
            language1: None,
            evse_id: None,
            group_id_token: None,
            language2: None,
            personal_message: None,
            custom_data: None,
        };
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            json!({"status": "Accepted"})
        );
        let parsed: IdTokenInfo = serde_json::from_value(json!({
            "status": "Accepted",
            "cacheExpiryDateTime": "2026-05-06T07:08:09Z"
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&parsed).unwrap()["cacheExpiryDateTime"],
            "2026-05-06T07:08:09.000Z"
        );
    }

    #[test]
    fn empty_optional_arrays_are_omitted() {
        let token = IdToken {
            additional_info: Some(Vec::new()),
            id_token: "T".into(),
            kind: IdTokenType::Local,
            custom_data: None,
        };
        assert_eq!(
            serde_json::to_value(&token).unwrap(),
            json!({"idToken": "T", "type": "Local"})
        );
    }

    #[test]
    fn unit_of_measure_defaults_are_not_materialized() {
        let parsed: UnitOfMeasure = serde_json::from_value(json!({})).unwrap();
        assert_eq!(parsed.unit, None);
        assert_eq!(parsed.multiplier, None);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), json!({}));
    }

    #[test]
    fn transaction_id_is_a_string() {
        let parsed: Transaction = serde_json::from_value(json!({"transactionId": "tx-1"})).unwrap();
        assert_eq!(parsed.transaction_id, "tx-1");
        assert!(serde_json::from_value::<Transaction>(json!({"transactionId": 1})).is_err());
    }

    #[test]
    fn wire_strings_are_exact() {
        assert_eq!(IdTokenType::Emaid.as_str(), "eMAID");
        assert_eq!(serde_json::to_value(Phase::L3L1).unwrap(), json!("L3-L1"));
        assert!(serde_json::from_value::<Phase>(json!("l1")).is_err());
    }
}
