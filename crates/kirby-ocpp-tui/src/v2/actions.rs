//! Tabs and the action catalogue of the OCPP 2.x UI.

use kirby_ocpp_core::scenario::Scenario;

/// The tabs, in key order (`1` to `6`). There is no Maintenance tab: its 1.6 actions have no
/// 2.x counterpart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab2 {
    Station,
    Evse,
    Transaction,
    Data,
    Logs,
    Scenarios,
}

impl Tab2 {
    pub const ALL: [Self; 6] = [
        Self::Station,
        Self::Evse,
        Self::Transaction,
        Self::Data,
        Self::Logs,
        Self::Scenarios,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Station => "Station",
            Self::Evse => "EVSE",
            Self::Transaction => "Transaction",
            Self::Data => "Data",
            Self::Logs => "Logs",
            Self::Scenarios => "Scenarios",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Self {
        Self::ALL[i % Self::ALL.len()]
    }

    pub fn next(self) -> Self {
        Self::from_index(self.index() + 1)
    }

    pub fn prev(self) -> Self {
        Self::from_index(self.index() + Self::ALL.len() - 1)
    }
}

/// Every action of the 2.x UI. Actions without a 2.x meaning (connector status, rules,
/// configuration keys, local list, profiles, certificates, diagnostics, firmware) have no id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionId2 {
    // Station
    Boot,
    Heartbeat,
    Reconnect,
    Disconnect,
    Connect,
    /// The device-model browser.
    DeviceModel,
    SecurityEvent,
    /// The commands waiting for the CSMS (a list; 2.x has no cancel).
    PendingCalls,
    // EVSE
    PlugToggle,
    FaultToggle,
    StatusNotification,
    // Transaction
    Authorize,
    /// Present a token at an EVSE (the outcome depends on the transaction start point).
    PresentToken,
    StopTransaction,
    Meter120,
    MeterCustom,
    ChargingState,
    // Data
    DataTransfer,
    VendorPing,
    RawCall,
    // Logs
    LogCopySelected,
    LogCopyVisible,
    LogFilter,
    LogSearch,
    LogPause,
    LogBottom,
    LogExport,
    LogClear,
    // Scenarios
    RunScenario(usize),
    LoadScenario,
    CancelScenario,
}

/// One entry of the catalogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDef2 {
    pub id: ActionId2,
    pub tab: Tab2,
    /// Accelerator, active only while `tab` is the current tab.
    pub key: Option<char>,
    pub label: String,
    pub desc: String,
}

/// The catalogue for the given scenarios (one entry per scenario on the Scenarios tab).
/// Slice S1 of M6 fills it.
#[must_use]
pub fn catalog2(scenarios: &[Scenario]) -> Vec<ActionDef2> {
    let _ = scenarios;
    Vec::new()
}
