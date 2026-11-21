use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ocpp::v2::types::{Component, CustomData, StatusInfo, Variable, payload, wire_enum};

wire_enum! {
    TariffSetStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
        TooManyElements = "TooManyElements",
        ConditionNotSupported = "ConditionNotSupported",
        DuplicateTariffId = "DuplicateTariffId",
    }
}

payload! {
    SetDefaultTariffRequest {
        pub evse_id: u64,
        pub tariff: Value,
    }
}

payload! {
    SetDefaultTariffResponse {
        pub status: TariffSetStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

action! {
    SetDefaultTariff = "SetDefaultTariff", SetDefaultTariffRequest => SetDefaultTariffResponse, InboundAction
}

wire_enum! {
    TariffGetStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
        NoTariff = "NoTariff",
    }
}

payload! {
    GetTariffsRequest {
        pub evse_id: u64,
    }
}

payload! {
    GetTariffsResponse {
        pub status: TariffGetStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tariff_assignments: Option<Vec<TariffAssignment>>,
    }
}

action! {
    GetTariffs = "GetTariffs", GetTariffsRequest => GetTariffsResponse, InboundAction
}

wire_enum! {
    TariffClearStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
        NoTariff = "NoTariff",
    }
}

payload! {
    ClearTariffsRequest {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evse_id: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tariff_ids: Option<Vec<String>>,
    }
}

payload! {
    ClearTariffsResponse {
        pub clear_tariffs_result: Vec<ClearTariffsResult>,
    }
}

action! {
    ClearTariffs = "ClearTariffs", ClearTariffsRequest => ClearTariffsResponse, InboundAction
}

wire_enum! {
    TariffChangeStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
        TooManyElements = "TooManyElements",
        ConditionNotSupported = "ConditionNotSupported",
        TxNotFound = "TxNotFound",
        NoCurrencyChange = "NoCurrencyChange",
    }
}

payload! {
    ChangeTransactionTariffRequest {
        pub transaction_id: String,
        pub tariff: Value,
    }
}

payload! {
    ChangeTransactionTariffResponse {
        pub status: TariffChangeStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

action! {
    ChangeTransactionTariff = "ChangeTransactionTariff", ChangeTransactionTariffRequest => ChangeTransactionTariffResponse, InboundAction
}

wire_enum! {
    TariffKind {
        DefaultTariff = "DefaultTariff",
        DriverTariff = "DriverTariff",
    }
}

payload! {
    TariffAssignment {
        pub tariff_id: String,
        pub tariff_kind: TariffKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evse_ids: Option<Vec<u32>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub id_tokens: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub valid_from: Option<DateTime<Utc>>,
    }
}

payload! {
    ClearTariffsResult {
        pub status: TariffClearStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tariff_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

wire_enum! {
    EventTrigger {
        Alerting = "Alerting",
        Delta = "Delta",
        Periodic = "Periodic",
    }
}

wire_enum! {
    EventNotificationKind {
        HardWiredNotification = "HardWiredNotification",
        HardWiredMonitor = "HardWiredMonitor",
        PreconfiguredMonitor = "PreconfiguredMonitor",
        CustomMonitor = "CustomMonitor",
    }
}

payload! {
    NotifyEventRequest {
        pub generated_at: DateTime<Utc>,
        pub seq_no: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tbc: Option<bool>,
        pub event_data: Vec<EventData>,
    }
}

payload! {
    EventData {
        pub event_id: u64,
        pub timestamp: DateTime<Utc>,
        pub trigger: EventTrigger,
        pub actual_value: String,
        pub event_notification_type: EventNotificationKind,
        pub component: Component,
        pub variable: Variable,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cause: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tech_code: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tech_info: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cleared: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub transaction_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub variable_monitoring_id: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub severity: Option<u64>,
    }
}

payload! {
    NotifyEventResponse {
    }
}

action! {
    NotifyEvent = "NotifyEvent", NotifyEventRequest => NotifyEventResponse, OutboundAction
}
