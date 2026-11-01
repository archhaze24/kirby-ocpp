//! Tabs and the action catalogue (the TS `actionsForTab`, plus the additions of the rewrite).

use kirby_ocpp_core::ids::ConnectorId;
use kirby_ocpp_core::scenario::Scenario;
use kirby_ocpp_core::station::Snapshot;
use kirby_ocpp_core::ws::ConnectionState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Station,
    Connector,
    Transaction,
    Maintenance,
    Data,
    Logs,
    Scenarios,
}

impl Tab {
    pub const ALL: [Self; 7] = [
        Self::Station,
        Self::Connector,
        Self::Transaction,
        Self::Maintenance,
        Self::Data,
        Self::Logs,
        Self::Scenarios,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Station => "Station",
            Self::Connector => "Connector",
            Self::Transaction => "Transaction",
            Self::Maintenance => "Maintenance",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionId {
    Boot,
    Heartbeat,
    Reconnect,
    Disconnect,
    Connect,
    AddConnector,
    RuleSet,
    RulesClear,
    BrowseConfig,
    BrowseLocalList,
    BrowseProfiles,
    PlugToggle,
    CycleStatus,
    SetStatus,
    StatusNotification,
    FaultToggle,
    Authorize,
    StartTx,
    StopTx,
    Meter120,
    MeterCustom,
    DiagOutcome,
    FwOutcome,
    DiagStatus,
    FwStatus,
    DataTransfer,
    VendorPing,
    RawCall,
    LogCopySelected,
    LogCopyVisible,
    LogFilter,
    LogSearch,
    LogPause,
    LogBottom,
    LogExport,
    LogClear,
    RunScenario(usize),
    LoadScenario,
    CancelScenario,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDef {
    pub id: ActionId,
    pub tab: Tab,
    /// Accelerator, active only while `tab` is the current tab.
    pub key: Option<char>,
    pub label: String,
    pub desc: String,
}

/// Keys that mean the same everywhere and so can never be accelerators.
pub const RESERVED_KEYS: &[char] = &[
    'q', 'j', 'k', '?', '/', ':', '[', ']', '1', '2', '3', '4', '5', '6', '7',
];

const SCENARIO_KEYS: [char; 4] = ['o', 'w', 's', 'i'];

fn def(id: ActionId, tab: Tab, key: char, label: &str, desc: &str) -> ActionDef {
    ActionDef {
        id,
        tab,
        key: Some(key),
        label: label.to_owned(),
        desc: desc.to_owned(),
    }
}

pub fn catalog(scenarios: &[Scenario]) -> Vec<ActionDef> {
    use ActionId as A;
    use Tab as T;
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
            A::AddConnector,
            T::Station,
            'a',
            "Add connector",
            "Add one more connector to the station.",
        ),
        def(
            A::RuleSet,
            T::Station,
            'm',
            "Response rule...",
            "Make the station misbehave on a CSMS command: Action=reject|delay:MS|error:Code|drop|ignore, optional *COUNT; Action=off removes a rule.",
        ),
        def(
            A::RulesClear,
            T::Station,
            'M',
            "Clear response rules",
            "Remove every response rule; the station answers CSMS commands normally again.",
        ),
        def(
            A::BrowseConfig,
            T::Station,
            'K',
            "Config keys...",
            "Browse the configuration keys and edit a writable one (like ChangeConfiguration, locally).",
        ),
        def(
            A::BrowseLocalList,
            T::Station,
            'L',
            "Local auth list...",
            "Browse the local auth list; add, edit, remove entries or clear it (bumps the list version).",
        ),
        def(
            A::BrowseProfiles,
            T::Station,
            'P',
            "Charging profiles...",
            "Browse installed charging profiles; install one from JSON, delete one or clear all.",
        ),
        def(
            A::PlugToggle,
            T::Connector,
            'p',
            "Plug / unplug",
            "Simulate the EV being plugged in or unplugged.",
        ),
        def(
            A::CycleStatus,
            T::Connector,
            's',
            "Cycle status",
            "Advance the connector status to the next one.",
        ),
        def(
            A::SetStatus,
            T::Connector,
            't',
            "Set status...",
            "Pick a connector status.",
        ),
        def(
            A::StatusNotification,
            T::Connector,
            'n',
            "StatusNotification",
            "Re-send the connector's current status.",
        ),
        def(
            A::FaultToggle,
            T::Connector,
            'f',
            "Fault / clear fault",
            "Inject a fault, or clear the current one.",
        ),
        def(
            A::Authorize,
            T::Transaction,
            'a',
            "Authorize...",
            "Send Authorize for an idTag.",
        ),
        def(
            A::StartTx,
            T::Transaction,
            't',
            "Start transaction...",
            "Start a transaction (connector + idTag).",
        ),
        def(
            A::StopTx,
            T::Transaction,
            'e',
            "Stop transaction...",
            "Stop the connector's transaction with a reason.",
        ),
        def(
            A::Meter120,
            T::Transaction,
            'm',
            "MeterValues +120 Wh",
            "Advance the meter by 120 Wh and send MeterValues.",
        ),
        def(
            A::MeterCustom,
            T::Transaction,
            'c',
            "MeterValues custom...",
            "MeterValues with a chosen Wh delta and measurands.",
        ),
        def(
            A::DiagOutcome,
            T::Maintenance,
            'd',
            "Diagnostics outcome...",
            "How the next simulated diagnostics upload ends.",
        ),
        def(
            A::FwOutcome,
            T::Maintenance,
            'u',
            "Firmware outcome...",
            "How the next simulated firmware update ends.",
        ),
        def(
            A::DiagStatus,
            T::Maintenance,
            'i',
            "DiagnosticsStatusNotification...",
            "Send a DiagnosticsStatusNotification.",
        ),
        def(
            A::FwStatus,
            T::Maintenance,
            'f',
            "FirmwareStatusNotification...",
            "Send a FirmwareStatusNotification.",
        ),
        def(
            A::DataTransfer,
            T::Data,
            'd',
            "DataTransfer...",
            "Send a DataTransfer (vendor, message id, data).",
        ),
        def(
            A::VendorPing,
            T::Data,
            'v',
            "Vendor ping",
            "Send a DataTransfer with data `ping` from the station vendor.",
        ),
        def(
            A::RawCall,
            T::Data,
            'r',
            "Raw CALL...",
            "Compose any CALL, validated against the OCPP schema.",
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
        v.push(ActionDef {
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

/// What the actions need to know to say whether they can run.
#[derive(Debug)]
pub struct Context<'a> {
    pub snapshot: &'a Snapshot,
    pub connector: ConnectorId,
    pub scenario_running: bool,
    pub log_has_selection: bool,
    pub log_visible: usize,
}

/// Why `id` cannot run right now; `None` when it can.
pub fn disabled_reason(id: ActionId, cx: &Context<'_>) -> Option<String> {
    use ActionId as A;
    let snap = cx.snapshot;
    let connected = snap.connection == ConnectionState::Connected;
    let need_conn = || (!connected).then(|| "not connected to the CSMS".to_owned());
    let conn = snap.connector(cx.connector);
    let idle = conn.is_none_or(|c| c.transaction.is_idle());
    match id {
        A::Boot
        | A::Heartbeat
        | A::DataTransfer
        | A::VendorPing
        | A::RawCall
        | A::DiagStatus
        | A::FwStatus => need_conn(),
        A::Connect => match snap.connection {
            ConnectionState::Connected => Some("already connected".to_owned()),
            ConnectionState::Connecting => Some("connection attempt in progress".to_owned()),
            _ => None,
        },
        A::Disconnect => (snap.connection == ConnectionState::Disconnected)
            .then(|| "already disconnected".to_owned()),
        A::PlugToggle | A::CycleStatus | A::SetStatus | A::StatusNotification | A::FaultToggle
            if conn.is_none() =>
        {
            Some("no connector selected".to_owned())
        }
        A::StartTx if !idle => Some(format!(
            "connector {} already has a transaction",
            cx.connector
        )),
        A::StopTx if idle => Some(format!("no transaction on connector {}", cx.connector)),
        A::LogCopySelected if !cx.log_has_selection => {
            Some("the log has no selected row".to_owned())
        }
        A::LogCopyVisible if cx.log_visible == 0 => Some("no rows to copy".to_owned()),
        A::RunScenario(_) | A::LoadScenario if cx.scenario_running => {
            Some("a scenario is already running".to_owned())
        }
        A::RulesClear if snap.response_rules.is_empty() => {
            Some("no response rules are active".to_owned())
        }
        A::CancelScenario if !cx.scenario_running => Some("no scenario is running".to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use kirby_ocpp_core::StationConfig;
    use kirby_ocpp_core::scenario::builtin;

    use super::*;

    #[test]
    fn accelerators_are_unique_per_tab_and_never_reserved() {
        let cat = catalog(&builtin::all());
        let mut seen = HashSet::new();
        for a in &cat {
            if let Some(k) = a.key {
                assert!(!RESERVED_KEYS.contains(&k), "{} uses reserved {k}", a.label);
                assert!(seen.insert((a.tab, k)), "duplicate {k} in {:?}", a.tab);
            }
        }
        assert_eq!(cat.iter().filter(|a| a.tab == Tab::Scenarios).count(), 6);
    }

    #[test]
    fn disabled_actions_explain_themselves() {
        let snap = Snapshot::initial(&StationConfig::new("ws://h/p"));
        let cx = Context {
            snapshot: &snap,
            connector: ConnectorId::new(1),
            scenario_running: false,
            log_has_selection: false,
            log_visible: 0,
        };
        assert_eq!(
            disabled_reason(ActionId::Boot, &cx).as_deref(),
            Some("not connected to the CSMS")
        );
        assert!(
            disabled_reason(ActionId::StopTx, &cx)
                .unwrap()
                .contains("no transaction")
        );
        assert!(disabled_reason(ActionId::Connect, &cx).is_none());
        assert!(disabled_reason(ActionId::CancelScenario, &cx).is_some());
        assert!(disabled_reason(ActionId::StartTx, &cx).is_none());
    }

    #[test]
    fn tab_navigation_wraps() {
        assert_eq!(Tab::Scenarios.next(), Tab::Station);
        assert_eq!(Tab::Station.prev(), Tab::Scenarios);
    }
}
