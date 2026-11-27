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
    Data,
    Logs,
    Scenarios,
    /// Last, so Logs and Scenarios keep the numbers they have for OCPP 2.x.
    Maintenance,
}

impl Tab {
    pub const ALL: [Self; 7] = [
        Self::Station,
        Self::Connector,
        Self::Transaction,
        Self::Data,
        Self::Logs,
        Self::Scenarios,
        Self::Maintenance,
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
    BrowseCertificates,
    RequestCertificate,
    SecurityEvent,
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
    /// The commands waiting for the Central System, with a way to cancel one.
    PendingCalls,
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
    'q', 'j', 'k', 'W', 'z', '?', '/', ':', '[', ']', '1', '2', '3', '4', '5', '6', '7',
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

/// The task group of an action in the unfiltered palette (see `PaletteRow::group`).
pub fn palette_group(id: ActionId) -> u8 {
    use ActionId as A;
    match id {
        A::Connect | A::Disconnect | A::Reconnect => 0,
        A::PlugToggle => 1,
        A::Authorize | A::StartTx | A::StopTx | A::Meter120 | A::MeterCustom => 2,
        A::Boot
        | A::Heartbeat
        | A::StatusNotification
        | A::CycleStatus
        | A::SetStatus
        | A::DataTransfer
        | A::VendorPing => 3,
        A::FaultToggle | A::RuleSet | A::RulesClear => 4,
        A::RunScenario(_) | A::LoadScenario | A::CancelScenario => 5,
        _ => 6,
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
            "Send BootNotification",
            "Send BootNotification.",
        ),
        def(
            A::Heartbeat,
            T::Station,
            'h',
            "Send Heartbeat",
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
        ActionDef {
            id: A::PendingCalls,
            tab: T::Station,
            // `W` works on every tab (it is in `RESERVED_KEYS`), so it is not a tab accelerator.
            key: None,
            label: "Show pending calls...".to_owned(),
            desc: "List the commands waiting for the Central System (key W); x cancels the selected one."
                .to_owned(),
        },
        def(
            A::RuleSet,
            T::Station,
            'm',
            "Set response rule...",
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
            "Edit config keys...",
            "Browse the configuration keys and edit a writable one (like ChangeConfiguration, locally).",
        ),
        def(
            A::BrowseLocalList,
            T::Station,
            'L',
            "Edit local auth list...",
            "Browse the local auth list; add, edit, remove entries or clear it (bumps the list version).",
        ),
        def(
            A::BrowseProfiles,
            T::Station,
            'P',
            "Edit charging profiles...",
            "Browse installed charging profiles; install one from JSON, delete one or clear all.",
        ),
        def(
            A::BrowseCertificates,
            T::Station,
            'Y',
            "View certificates...",
            "Browse the installed certificates (read-only): roots, manufacturer roots and the station certificate.",
        ),
        def(
            A::RequestCertificate,
            T::Station,
            'C',
            "Request certificate",
            "Create a key pair and a CSR and send SignCertificate (Security Whitepaper); the CSMS answers with CertificateSigned later.",
        ),
        def(
            A::SecurityEvent,
            T::Station,
            'S',
            "Security event...",
            "Report a security event (SecurityEventNotification): pick a type and an optional techInfo.",
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
            "Cycle connector status",
            "Advance the connector status to the next one.",
        ),
        def(
            A::SetStatus,
            T::Connector,
            't',
            "Set connector status...",
            "Pick a connector status.",
        ),
        def(
            A::StatusNotification,
            T::Connector,
            'n',
            "Send StatusNotification",
            "Re-send the connector's current status.",
        ),
        def(
            A::FaultToggle,
            T::Connector,
            'f',
            "Inject or clear fault",
            "Inject a fault, or clear the current one.",
        ),
        def(
            A::Authorize,
            T::Transaction,
            'a',
            "Authorize token...",
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
            "Send meter +120 Wh",
            "Advance the meter by 120 Wh and send MeterValues.",
        ),
        def(
            A::MeterCustom,
            T::Transaction,
            'c',
            "Send custom meter values...",
            "MeterValues with a chosen Wh delta and measurands.",
        ),
        def(
            A::DiagOutcome,
            T::Maintenance,
            'd',
            "Set diagnostics outcome...",
            "How the next simulated diagnostics upload ends.",
        ),
        def(
            A::FwOutcome,
            T::Maintenance,
            'u',
            "Set firmware outcome...",
            "How the next simulated firmware update ends.",
        ),
        def(
            A::DiagStatus,
            T::Maintenance,
            'i',
            "Send DiagnosticsStatusNotification...",
            "Send a DiagnosticsStatusNotification.",
        ),
        def(
            A::FwStatus,
            T::Maintenance,
            'f',
            "Send FirmwareStatusNotification...",
            "Send a FirmwareStatusNotification.",
        ),
        def(
            A::DataTransfer,
            T::Data,
            'd',
            "Send DataTransfer...",
            "Send a DataTransfer (vendor, message id, data).",
        ),
        def(
            A::VendorPing,
            T::Data,
            'v',
            "Send vendor ping",
            "Send a DataTransfer with data `ping` from the station vendor.",
        ),
        def(
            A::RawCall,
            T::Data,
            'r',
            "Send raw CALL...",
            "Compose any CALL, validated against the OCPP schema.",
        ),
        def(
            A::LogCopySelected,
            T::Logs,
            'y',
            "Copy selected row",
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
            "Cycle log filter",
            "all / calls / errors / station / csms.",
        ),
        def(
            A::LogSearch,
            T::Logs,
            's',
            "Search log...",
            "Filter rows by text (action, summary, payload).",
        ),
        def(
            A::LogPause,
            T::Logs,
            'p',
            "Pause or follow log",
            "Stop or resume following the newest row.",
        ),
        def(
            A::LogBottom,
            T::Logs,
            'G',
            "Jump to newest row",
            "Select the newest row and follow.",
        ),
        def(
            A::LogExport,
            T::Logs,
            'e',
            "Export log...",
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
        "Run scenario file...",
        "Run a scenario TOML file.",
    ));
    v.push(def(
        A::CancelScenario,
        T::Scenarios,
        'x',
        "Cancel scenario",
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
        | A::RequestCertificate
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
        let builtins = builtin::all();
        let cat = catalog(&builtins);
        let mut seen = HashSet::new();
        for a in &cat {
            if let Some(k) = a.key {
                assert!(!RESERVED_KEYS.contains(&k), "{} uses reserved {k}", a.label);
                assert!(seen.insert((a.tab, k)), "duplicate {k} in {:?}", a.tab);
            }
        }
        // One run action per built-in plus load and cancel; only the first four have a key.
        assert_eq!(
            cat.iter().filter(|a| a.tab == Tab::Scenarios).count(),
            builtins.len() + 2
        );
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
        assert_eq!(Tab::Maintenance.next(), Tab::Station);
        assert_eq!(Tab::Station.prev(), Tab::Maintenance);
    }

    #[test]
    fn tab_numbers_match_the_ocpp_2x_tabs_up_to_scenarios() {
        let titles: Vec<&str> = Tab::ALL.iter().map(|t| t.title()).collect();
        assert_eq!(
            titles,
            [
                "Station",
                "Connector",
                "Transaction",
                "Data",
                "Logs",
                "Scenarios",
                "Maintenance"
            ]
        );
        assert_eq!(Tab::Logs.index() + 1, 5);
        assert_eq!(Tab::Scenarios.index() + 1, 6);
    }
}
