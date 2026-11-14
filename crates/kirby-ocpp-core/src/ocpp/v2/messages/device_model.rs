use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ocpp::v2::types::{
    AttributeType, Component, CustomData, StatusInfo, Variable, helpers, payload, wire_enum,
};

wire_enum! {
    /// Data type of a variable (`DataEnumType`).
    DataType {
        String = "string",
        Decimal = "decimal",
        Integer = "integer",
        DateTime = "dateTime",
        Boolean = "boolean",
        OptionList = "OptionList",
        SequenceList = "SequenceList",
        MemberList = "MemberList",
    }
}

wire_enum! {
    /// Mutability of a variable attribute (`MutabilityEnumType`).
    Mutability {
        ReadOnly = "ReadOnly",
        WriteOnly = "WriteOnly",
        ReadWrite = "ReadWrite",
    }
}

wire_enum! {
    /// Inventory scope of `GetBaseReport` (`ReportBaseEnumType`).
    ReportBase {
        ConfigurationInventory = "ConfigurationInventory",
        FullInventory = "FullInventory",
        SummaryInventory = "SummaryInventory",
    }
}

wire_enum! {
    /// Per-variable result of `GetVariables` (`GetVariableStatusEnumType`).
    GetVariableStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
        UnknownComponent = "UnknownComponent",
        UnknownVariable = "UnknownVariable",
        NotSupportedAttributeType = "NotSupportedAttributeType",
    }
}

wire_enum! {
    /// Per-variable result of `SetVariables` (`SetVariableStatusEnumType`).
    SetVariableStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
        UnknownComponent = "UnknownComponent",
        UnknownVariable = "UnknownVariable",
        NotSupportedAttributeType = "NotSupportedAttributeType",
        RebootRequired = "RebootRequired",
    }
}

wire_enum! {
    /// Result of `GetBaseReport` (`GenericDeviceModelStatusEnumType`).
    GenericDeviceModelStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
        NotSupported = "NotSupported",
        EmptyResultSet = "EmptyResultSet",
    }
}

payload! {
    /// One attribute of a reported variable (`VariableAttributeType`).
    VariableAttribute {
        /// Absent means `Actual`; the default is not filled in.
        #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
        pub kind: Option<AttributeType>,
        /// Max 2500 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub value: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub mutability: Option<Mutability>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub persistent: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub constant: Option<bool>,
    }
}

payload! {
    /// Characteristics of a reported variable (`VariableCharacteristicsType`).
    /// The limits are decoded as `f64`.
    VariableCharacteristics {
        /// Max 16 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub unit: Option<String>,
        pub data_type: DataType,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub min_limit: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub max_limit: Option<f64>,
        /// Comma-separated list, max 1000 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub values_list: Option<String>,
        pub supports_monitoring: bool,
    }
}

payload! {
    /// One variable in a report (`ReportDataType`).
    ReportData {
        pub component: Component,
        pub variable: Variable,
        /// One to four attributes.
        pub variable_attribute: Vec<VariableAttribute>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub variable_characteristics: Option<VariableCharacteristics>,
    }
}

payload! {
    /// Variable to read (`GetVariableDataType`).
    GetVariableData {
        /// Absent means `Actual`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub attribute_type: Option<AttributeType>,
        pub component: Component,
        pub variable: Variable,
    }
}

payload! {
    /// Result for one variable of `GetVariables` (`GetVariableResultType`).
    GetVariableResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub attribute_status_info: Option<StatusInfo>,
        pub attribute_status: GetVariableStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub attribute_type: Option<AttributeType>,
        /// Max 2500 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub attribute_value: Option<String>,
        pub component: Component,
        pub variable: Variable,
    }
}

payload! {
    /// Variable to write (`SetVariableDataType`).
    SetVariableData {
        /// Absent means `Actual`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub attribute_type: Option<AttributeType>,
        /// Max 1000 characters.
        pub attribute_value: String,
        pub component: Component,
        pub variable: Variable,
    }
}

payload! {
    /// Result for one variable of `SetVariables` (`SetVariableResultType`).
    SetVariableResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub attribute_type: Option<AttributeType>,
        pub attribute_status: SetVariableStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub attribute_status_info: Option<StatusInfo>,
        pub component: Component,
        pub variable: Variable,
    }
}

payload! {
    /// `GetVariables.req`.
    GetVariablesRequest {
        /// At least one entry.
        pub get_variable_data: Vec<GetVariableData>,
    }
}

payload! {
    /// `GetVariables.conf`.
    GetVariablesResponse {
        /// At least one entry.
        pub get_variable_result: Vec<GetVariableResult>,
    }
}

payload! {
    /// `SetVariables.req`.
    SetVariablesRequest {
        /// At least one entry.
        pub set_variable_data: Vec<SetVariableData>,
    }
}

payload! {
    /// `SetVariables.conf`.
    SetVariablesResponse {
        /// At least one entry.
        pub set_variable_result: Vec<SetVariableResult>,
    }
}

payload! {
    /// `GetBaseReport.req`.
    GetBaseReportRequest {
        pub request_id: i32,
        pub report_base: ReportBase,
    }
}

payload! {
    /// `GetBaseReport.conf`.
    GetBaseReportResponse {
        pub status: GenericDeviceModelStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

payload! {
    /// `NotifyReport.req`.
    NotifyReportRequest {
        /// The `requestId` of the `GetBaseReport` (or similar) request.
        pub request_id: i32,
        #[serde(with = "helpers::ts")]
        pub generated_at: DateTime<Utc>,
        /// Absent or non-empty.
        #[serde(default, skip_serializing_if = "helpers::omit_empty")]
        pub report_data: Option<Vec<ReportData>>,
        /// "To be continued": more report parts follow. Not defaulted when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tbc: Option<bool>,
        pub seq_no: i32,
    }
}

payload! {
    /// `NotifyReport.conf`: no fields besides `customData`.
    NotifyReportResponse {}
}

action! {
    /// `GetVariables` (CSMS -> station).
    GetVariables = "GetVariables", GetVariablesRequest => GetVariablesResponse, InboundAction
}
action! {
    /// `SetVariables` (CSMS -> station).
    SetVariables = "SetVariables", SetVariablesRequest => SetVariablesResponse, InboundAction
}
action! {
    /// `GetBaseReport` (CSMS -> station).
    GetBaseReport = "GetBaseReport", GetBaseReportRequest => GetBaseReportResponse, InboundAction
}
action! {
    /// `NotifyReport` (station -> CSMS).
    NotifyReport = "NotifyReport", NotifyReportRequest => NotifyReportResponse, OutboundAction
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::ocpp::v2::test_support::{check_action, check_enum};

    fn cd() -> Value {
        json!({"vendorId": "acme", "x": {"y": [1, null, "z"]}})
    }

    fn status_info() -> Value {
        json!({"customData": cd(), "reasonCode": "Reason", "additionalInfo": "detail"})
    }

    fn component() -> Value {
        json!({
            "customData": cd(),
            "evse": {"customData": cd(), "id": 1, "connectorId": 2},
            "name": "Connector",
            "instance": "main"
        })
    }

    fn variable() -> Value {
        json!({"customData": cd(), "name": "Available", "instance": "i"})
    }

    #[test]
    fn enums_match_schema() {
        check_enum::<DataType>("NotifyReportRequest.json", "DataEnumType");
        check_enum::<Mutability>("NotifyReportRequest.json", "MutabilityEnumType");
        check_enum::<ReportBase>("GetBaseReportRequest.json", "ReportBaseEnumType");
        check_enum::<GetVariableStatus>("GetVariablesResponse.json", "GetVariableStatusEnumType");
        check_enum::<SetVariableStatus>("SetVariablesResponse.json", "SetVariableStatusEnumType");
        check_enum::<GenericDeviceModelStatus>(
            "GetBaseReportResponse.json",
            "GenericDeviceModelStatusEnumType",
        );
    }

    #[test]
    fn get_variables() {
        check_action::<GetVariables>(
            &json!({
                "customData": cd(),
                "getVariableData": [
                    {
                        "customData": cd(),
                        "attributeType": "MaxSet",
                        "component": component(),
                        "variable": variable()
                    },
                    {"component": {"name": "C"}, "variable": {"name": "V"}}
                ]
            }),
            &json!({
                "customData": cd(),
                "getVariableResult": [
                    {
                        "customData": cd(),
                        "attributeStatusInfo": status_info(),
                        "attributeStatus": "NotSupportedAttributeType",
                        "attributeType": "Target",
                        "attributeValue": "42",
                        "component": component(),
                        "variable": variable()
                    },
                    {
                        "attributeStatus": "UnknownVariable",
                        "component": {"name": "C"},
                        "variable": {"name": "V"}
                    }
                ]
            }),
        );
        check_action::<GetVariables>(
            &json!({"getVariableData": [{"component": {"name": "C"}, "variable": {"name": "V"}}]}),
            &json!({"getVariableResult": [{
                "attributeStatus": "Accepted",
                "component": {"name": "C"},
                "variable": {"name": "V"}
            }]}),
        );
    }

    #[test]
    fn set_variables() {
        check_action::<SetVariables>(
            &json!({
                "customData": cd(),
                "setVariableData": [{
                    "customData": cd(),
                    "attributeType": "Actual",
                    "attributeValue": "60",
                    "component": component(),
                    "variable": variable()
                }]
            }),
            &json!({
                "customData": cd(),
                "setVariableResult": [{
                    "customData": cd(),
                    "attributeType": "Actual",
                    "attributeStatus": "RebootRequired",
                    "attributeStatusInfo": status_info(),
                    "component": component(),
                    "variable": variable()
                }]
            }),
        );
        check_action::<SetVariables>(
            &json!({"setVariableData": [{
                "attributeValue": "1",
                "component": {"name": "C"},
                "variable": {"name": "V"}
            }]}),
            &json!({"setVariableResult": [{
                "attributeStatus": "Accepted",
                "component": {"name": "C"},
                "variable": {"name": "V"}
            }]}),
        );
    }

    #[test]
    fn get_base_report() {
        check_action::<GetBaseReport>(
            &json!({"customData": cd(), "requestId": 5, "reportBase": "FullInventory"}),
            &json!({"customData": cd(), "status": "EmptyResultSet", "statusInfo": status_info()}),
        );
        check_action::<GetBaseReport>(
            &json!({"requestId": 0, "reportBase": "SummaryInventory"}),
            &json!({"status": "Accepted"}),
        );
    }

    #[test]
    fn notify_report() {
        check_action::<NotifyReport>(
            &json!({
                "customData": cd(),
                "requestId": 5,
                "generatedAt": "2026-05-06T07:08:09.123Z",
                "reportData": [{
                    "customData": cd(),
                    "component": component(),
                    "variable": variable(),
                    "variableAttribute": [
                        {
                            "customData": cd(),
                            "type": "Actual",
                            "value": "yes",
                            "mutability": "ReadWrite",
                            "persistent": true,
                            "constant": false
                        },
                        {"type": "MinSet", "value": "0"}
                    ],
                    "variableCharacteristics": {
                        "customData": cd(),
                        "unit": "A",
                        "dataType": "decimal",
                        "minLimit": 0.5,
                        "maxLimit": 32.5,
                        "valuesList": "1,2,3",
                        "supportsMonitoring": true
                    }
                }],
                "tbc": true,
                "seqNo": 1
            }),
            &json!({"customData": cd()}),
        );
        check_action::<NotifyReport>(
            &json!({
                "requestId": 5,
                "generatedAt": "2026-05-06T07:08:09.000Z",
                "seqNo": 0
            }),
            &json!({}),
        );
    }

    #[test]
    fn notify_report_attribute_without_type_stays_without_type() {
        let parsed: VariableAttribute = serde_json::from_value(json!({"value": "1"})).unwrap();
        assert_eq!(parsed.kind, None);
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            json!({"value": "1"})
        );
    }
}
