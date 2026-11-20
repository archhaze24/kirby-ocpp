//! Tabs and the action catalogue of the OCPP 2.x UI.

use kirby_ocpp_core::scenario::Scenario;
use kirby_ocpp_core::station2::Snapshot2;
use kirby_ocpp_core::ws::ConnectionState;

use crate::palette::PaletteRow;

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
    /// The commands waiting for the CSMS.
    PendingCalls,
    // EVSE
    PlugToggle,
    FaultToggle,
    StatusNotification,
    ExternalLimit,
    ClearExternalLimit,
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

/// Accelerators of the first built-in scenarios; the rest are reached with the arrow keys or
/// the palette.
const SCENARIO_KEYS: [char; 4] = ['o', 'w', 's', 'i'];

fn def(id: ActionId2, tab: Tab2, key: char, label: &str, desc: &str) -> ActionDef2 {
    ActionDef2 {
        id,
        tab,
        key: Some(key),
        label: label.to_owned(),
        desc: desc.to_owned(),
    }
}

/// The catalogue for the given scenarios (one entry per scenario on the Scenarios tab).
///
/// Letters keep their 1.6 meaning when the action is the same. Nothing is listed that has no
/// OCPP 2.0.1 counterpart: connector status is derived from the EVSE state, rules name 1.6
/// actions, configuration keys are the device model, and the local list, charging profiles,
/// certificates, diagnostics and firmware are not part of the emulated station.
#[must_use]
pub fn catalog2(scenarios: &[Scenario]) -> Vec<ActionDef2> {
    use ActionId2 as A;
    use Tab2 as T;
    let mut v = vec![
        def(
            A::Boot,
            T::Station,
            'b',
            "BootNotification",
            "Send BootNotification.",
        ),
        def(
            A::Heartbeat,
            T::Station,
            'h',
            "Heartbeat",
            "Send Heartbeat.",
        ),
        def(
            A::Reconnect,
            T::Station,
            'r',
            "Reconnect",
            "Close the connection and open it again.",
        ),
        def(
            A::Disconnect,
            T::Station,
            'x',
            "Disconnect",
            "Close the connection; no automatic reconnect.",
        ),
        def(
            A::Connect,
            T::Station,
            'c',
            "Connect",
            "Open the connection to the CSMS.",
        ),
        def(
            A::DeviceModel,
            T::Station,
            'K',
            "Device model...",
            "Browse the station-level variables of the device model and edit a ReadWrite one (like SetVariables, locally).",
        ),
        def(
            A::SecurityEvent,
            T::Station,
            'S',
            "Security event...",
            "Report a security event (SecurityEventNotification): pick a type or type your own, and an optional techInfo.",
        ),
        ActionDef2 {
            id: A::PendingCalls,
            tab: T::Station,
            // `W` works on every tab (it is in `RESERVED_KEYS`), so it is not a tab accelerator.
            key: None,
            label: "Pending calls...".to_owned(),
            desc: "List the commands waiting for the CSMS (key W); OCPP 2.0.1 commands cannot be cancelled."
                .to_owned(),
        },
        def(
            A::PlugToggle,
            T::Evse,
            'p',
            "Plug / unplug",
            "Simulate the EV being plugged in or unplugged at the selected EVSE.",
        ),
        def(
            A::FaultToggle,
            T::Evse,
            'f',
            "Fault / clear fault",
            "Inject a fault on every connector of the selected EVSE, or clear it.",
        ),
        def(
            A::StatusNotification,
            T::Evse,
            'n',
            "StatusNotification",
            "Re-send the current connector status of the selected EVSE.",
        ),
        def(
            A::ExternalLimit,
            T::Evse,
            'l',
            "Set external limit...",
            "Apply a limit of an external system to an EVSE or the whole station, in A or W; the CSMS is notified.",
        ),
        def(
            A::ClearExternalLimit,
            T::Evse,
            'L',
            "Clear external limit...",
            "Release the external limit of an EVSE or the whole station; the CSMS is notified.",
        ),
        def(
            A::Authorize,
            T::Transaction,
            'a',
            "Authorize...",
            "Send Authorize for an idToken.",
        ),
        def(
            A::PresentToken,
            T::Transaction,
            't',
            "Present token...",
            "Present an idToken at an EVSE; whether a transaction starts depends on TxStartPoint.",
        ),
        def(
            A::StopTransaction,
            T::Transaction,
            'e',
            "Stop transaction...",
            "Stop the transaction of the selected EVSE with a reason.",
        ),
        def(
            A::Meter120,
            T::Transaction,
            'm',
            "Meter +120 Wh",
            "Advance the meter of the selected EVSE by 120 Wh and send a sample.",
        ),
        def(
            A::MeterCustom,
            T::Transaction,
            'c',
            "Meter custom...",
            "Advance the meter by a chosen Wh delta, with or without a sample.",
        ),
        def(
            A::ChargingState,
            T::Transaction,
            'g',
            "Charging state...",
            "Pick the charging state of the transaction (ChargingStateEnumType).",
        ),
        def(
            A::DataTransfer,
            T::Data,
            'd',
            "DataTransfer...",
            "Send a DataTransfer (vendor, message id, data as any JSON value).",
        ),
        def(
            A::VendorPing,
            T::Data,
            'v',
            "Vendor ping",
            "Send a DataTransfer with only the vendor id of the station.",
        ),
        def(
            A::RawCall,
            T::Data,
            'r',
            "Raw CALL...",
            "Compose any station-to-CSMS CALL, validated against the OCPP 2.0.1 schema.",
        ),
        def(
            A::LogCopySelected,
            T::Logs,
            'y',
            "Copy selected frame",
            "Copy the selected log row as pretty JSON.",
        ),
        def(
            A::LogCopyVisible,
            T::Logs,
            'Y',
            "Copy visible log",
            "Copy every row that passes the filter and search.",
        ),
        def(
            A::LogFilter,
            T::Logs,
            'f',
            "Cycle filter",
            "all / calls / errors / station / csms.",
        ),
        def(
            A::LogSearch,
            T::Logs,
            's',
            "Search...",
            "Filter rows by text (action, summary, payload).",
        ),
        def(
            A::LogPause,
            T::Logs,
            'p',
            "Pause / follow",
            "Stop or resume following the newest row.",
        ),
        def(
            A::LogBottom,
            T::Logs,
            'G',
            "Jump to newest",
            "Select the newest row and follow.",
        ),
        def(
            A::LogExport,
            T::Logs,
            'e',
            "Export JSONL...",
            "Write the whole log to a JSONL file.",
        ),
        def(
            A::LogClear,
            T::Logs,
            'c',
            "Clear log...",
            "Remove every log row (asks first).",
        ),
    ];
    for (i, s) in scenarios.iter().enumerate() {
        v.push(ActionDef2 {
            id: A::RunScenario(i),
            tab: T::Scenarios,
            key: SCENARIO_KEYS.get(i).copied(),
            label: format!("Run: {}", s.name),
            desc: s.description.clone(),
        });
    }
    v.push(def(
        A::LoadScenario,
        T::Scenarios,
        'l',
        "Load from file...",
        "Run a scenario TOML file.",
    ));
    v.push(def(
        A::CancelScenario,
        T::Scenarios,
        'x',
        "Cancel running scenario",
        "Stop the running scenario.",
    ));
    v
}

/// The palette rows of a catalogue; palette results index into the catalogue.
pub(super) fn palette_rows(catalog: &[ActionDef2]) -> Vec<PaletteRow<'_>> {
    catalog
        .iter()
        .map(|a| PaletteRow {
            label: &a.label,
            desc: &a.desc,
            tab: a.tab.title(),
        })
        .collect()
}

/// What the actions need to know to say whether they can run.
#[derive(Debug)]
pub(super) struct Context2<'a> {
    pub snapshot: &'a Snapshot2,
    /// The selected EVSE (0 when the station has none).
    pub evse: u32,
    pub scenario_running: bool,
    pub log_has_selection: bool,
    pub log_visible: usize,
}

/// Why `id` cannot run right now; `None` when it can.
pub(super) fn disabled_reason2(id: ActionId2, cx: &Context2<'_>) -> Option<String> {
    use ActionId2 as A;
    let snap = cx.snapshot;
    let evse = snap.evse(cx.evse);
    let need_conn = || {
        (snap.connection != ConnectionState::Connected)
            .then(|| "not connected to the CSMS".to_owned())
    };
    let need_evse = || evse.is_none().then(|| "no EVSE selected".to_owned());
    let need_tx = || {
        if evse.is_some_and(|e| e.transaction.is_some()) {
            None
        } else {
            Some(format!("no transaction on EVSE {}", cx.evse))
        }
    };
    match id {
        A::Boot
        | A::Heartbeat
        | A::SecurityEvent
        | A::Authorize
        | A::DataTransfer
        | A::VendorPing
        | A::RawCall => need_conn(),
        A::StatusNotification => need_conn().or_else(need_evse),
        A::Connect => match snap.connection {
            ConnectionState::Connected => Some("already connected".to_owned()),
            ConnectionState::Connecting => Some("connection attempt in progress".to_owned()),
            _ => None,
        },
        A::Disconnect => (snap.connection == ConnectionState::Disconnected)
            .then(|| "already disconnected".to_owned()),
        // A token may come before the cable: `TxStartPoint` decides what it starts (E01, E02).
        A::PlugToggle | A::FaultToggle | A::Meter120 | A::MeterCustom | A::PresentToken => {
            need_evse()
        }
        A::StopTransaction | A::ChargingState => need_evse().or_else(need_tx),
        A::LogCopySelected if !cx.log_has_selection => {
            Some("the log has no selected row".to_owned())
        }
        A::LogCopyVisible if cx.log_visible == 0 => Some("no rows to copy".to_owned()),
        A::RunScenario(_) | A::LoadScenario if cx.scenario_running => {
            Some("a scenario is already running".to_owned())
        }
        A::CancelScenario if !cx.scenario_running => Some("no scenario is running".to_owned()),
        _ => None,
    }
}
