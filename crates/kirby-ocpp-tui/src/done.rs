//! Status-line texts for finished emulator commands, in plain words: `Plugged in connector 1`
//! instead of the internal command name.
//!
//! The text is built when the command is sent, from the command and the snapshot at that time
//! (a fault, for example, must know whether it ends a running transaction).

use kirby_ocpp_core::ids::ConnectorId;
use kirby_ocpp_core::ocpp::v2::types::WireEnum;
use kirby_ocpp_core::station::{Command, ConnectorSnapshot, Snapshot};
use kirby_ocpp_core::station2::{Command2, Snapshot2};

use crate::text::sanitize;

/// What the status line says when `command` succeeded; `None` leaves the generic text.
#[must_use]
pub fn done_text(command: &Command, snapshot: &Snapshot) -> Option<String> {
    let running = |c: ConnectorId| snapshot.connector(c).and_then(ConnectorSnapshot::tx_id);
    Some(match command {
        Command::Connect => "Connecting to the CSMS".to_owned(),
        Command::Disconnect => "Disconnected from the CSMS".to_owned(),
        Command::Reconnect => "Reconnecting to the CSMS".to_owned(),
        Command::Boot => "Sent BootNotification".to_owned(),
        Command::Heartbeat => "Sent Heartbeat".to_owned(),
        Command::AddConnector => "Added a connector".to_owned(),
        Command::PlugIn { connector } => format!("Plugged in connector {connector}"),
        Command::Unplug { connector } => match running(*connector) {
            Some(tx) => format!("Unplugged connector {connector}. Transaction {tx} ended"),
            None => format!("Unplugged connector {connector}"),
        },
        Command::SetConnectorStatus { connector, status } => {
            format!("Connector {connector} status set to {}", status.as_str())
        }
        Command::CycleConnectorStatus { connector } => {
            format!("Cycled the status of connector {connector}")
        }
        Command::StatusNotification { connector } => {
            format!("Sent StatusNotification for connector {connector}")
        }
        Command::Fault {
            connector,
            error_code,
            ..
        } => {
            let fault = format!("Fault {} on connector {connector}", error_code.as_str());
            match running(*connector) {
                Some(tx) => format!("{fault}. Transaction {tx} ended by the fault"),
                None => fault,
            }
        }
        Command::ClearFault { connector } => format!("Cleared the fault on connector {connector}"),
        Command::StopTransaction {
            connector, reason, ..
        } => format!(
            "Stopped the transaction on connector {connector} ({})",
            reason.as_str()
        ),
        Command::MeterValues {
            wh_delta,
            measurands,
            ..
        } => {
            let extra = if measurands.is_some() {
                ", custom measurands"
            } else {
                ""
            };
            format!("Sent meter values (+{wh_delta} Wh{extra})")
        }
        Command::DataTransfer { .. } => "Sent DataTransfer".to_owned(),
        Command::DiagnosticsStatusNotification { status } => {
            format!("Sent DiagnosticsStatusNotification ({})", status.as_str())
        }
        Command::FirmwareStatusNotification { status } => {
            format!("Sent FirmwareStatusNotification ({})", status.as_str())
        }
        Command::SetDiagnosticsOutcome { outcome } => {
            format!("The next diagnostics upload will end as {outcome:?}")
        }
        Command::SetFirmwareOutcome { outcome } => {
            format!("The next firmware update will end as {outcome:?}")
        }
        Command::SendRaw { action, .. } => format!("Sent {}", sanitize(action)),
        Command::SetResponseRule { rule } => format!("Response rule set: {rule}"),
        Command::ClearResponseRules { action: Some(a) } => {
            format!("Removed the response rule for {}", sanitize(a))
        }
        Command::ClearResponseRules { action: None } => "Cleared the response rules".to_owned(),
        Command::SetConfiguration { key, .. } => format!("Set {}", sanitize(key)),
        Command::SetLocalListEntry { id_tag, .. } => {
            format!("Local list: saved {}", sanitize(id_tag.as_str()))
        }
        Command::RemoveLocalListEntry { id_tag } => {
            format!("Local list: removed {}", sanitize(id_tag.as_str()))
        }
        Command::ClearLocalList => "Cleared the local list".to_owned(),
        Command::InstallChargingProfile { profile, .. } => {
            format!("Installed charging profile {}", profile.charging_profile_id)
        }
        Command::ClearChargingProfiles { id: Some(id) } => {
            format!("Removed charging profile {id}")
        }
        Command::ClearChargingProfiles { id: None } => "Cleared the charging profiles".to_owned(),
        Command::RequestCertificate => "Sent SignCertificate".to_owned(),
        Command::SendSecurityEvent { event_type, .. } => {
            format!("Reported security event {}", sanitize(event_type))
        }
        // Authorize, StartTransaction and the rest have their own outcome texts.
        _ => return None,
    })
}

/// [`done_text`] for OCPP 2.x. `commands` are the steps of one effect: a meter action is an
/// advance followed by a sample.
#[must_use]
pub fn done_text2(commands: &[Command2], snapshot: &Snapshot2) -> Option<String> {
    let running = |evse: u32| {
        snapshot
            .evse(evse)
            .and_then(|e| e.transaction.as_ref())
            .map(|t| sanitize(&t.transaction_id))
    };
    if let Some(Command2::AdvanceMeter { wh_delta, .. }) = commands.first() {
        let sampled = commands
            .iter()
            .any(|c| matches!(c, Command2::SampleMeter { .. }));
        return Some(if sampled {
            format!("Sent meter values (+{wh_delta} Wh)")
        } else {
            format!("Advanced the meter by {wh_delta} Wh")
        });
    }
    Some(match commands.first()? {
        Command2::Connect => "Connecting to the CSMS".to_owned(),
        Command2::Disconnect => "Disconnected from the CSMS".to_owned(),
        Command2::Reconnect => "Reconnecting to the CSMS".to_owned(),
        Command2::Boot => "Sent BootNotification".to_owned(),
        Command2::Heartbeat => "Sent Heartbeat".to_owned(),
        Command2::PlugIn { evse } => format!("Plugged in EVSE {evse}"),
        Command2::Unplug { evse } => match running(*evse) {
            Some(tx) => format!("Unplugged EVSE {evse}. Transaction {tx} ended"),
            None => format!("Unplugged EVSE {evse}"),
        },
        Command2::Fault { evse, .. } => match running(*evse) {
            Some(tx) => format!("Fault on EVSE {evse}. Transaction {tx} ended by the fault"),
            None => format!("Fault on EVSE {evse}"),
        },
        Command2::ClearFault { evse, .. } => format!("Cleared the fault on EVSE {evse}"),
        Command2::StatusNotification { evse, connector } => {
            format!("Sent StatusNotification for EVSE {evse} connector {connector}")
        }
        Command2::StopTransaction { evse, reason } => {
            format!(
                "Stopped the transaction on EVSE {evse} ({})",
                reason.as_str()
            )
        }
        Command2::SetChargingState { evse, state } => {
            format!("EVSE {evse} charging state set to {}", state.as_str())
        }
        Command2::SendSecurityEvent { event_type, .. } => {
            format!("Reported security event {}", sanitize(event_type))
        }
        Command2::DataTransfer { .. } => "Sent DataTransfer".to_owned(),
        Command2::SendRaw { action, .. } => format!("Sent {}", sanitize(action)),
        Command2::SetExternalLimit { evse, .. } => format!("Set the external limit on EVSE {evse}"),
        Command2::ClearExternalLimit { evse } => {
            format!("Cleared the external limit on EVSE {evse}")
        }
        Command2::SetPriorityCharging { evse, activate } => format!(
            "Priority charging {} on EVSE {evse}",
            if *activate { "on" } else { "off" }
        ),
        Command2::SetTransactionLimit { evse, .. } => {
            format!("Set the transaction limit on EVSE {evse}")
        }
        // Authorize and PresentToken have their own outcome texts.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use kirby_ocpp_core::StationConfig;
    use kirby_ocpp_core::ids::{IdTag, TxId};
    use kirby_ocpp_core::ocpp::types::{ChargePointErrorCode, StopReason};
    use kirby_ocpp_core::station::TransactionState;
    use kirby_ocpp_core::station::transaction::ActiveTransaction;

    use super::*;

    fn snapshot_with_tx() -> Snapshot {
        let mut snap = Snapshot::initial(&StationConfig::new("ws://h/p"));
        snap.connectors[0].transaction = TransactionState::Active(ActiveTransaction {
            tx_id: TxId::Csms(7),
            id_tag: IdTag::new("T").unwrap(),
            started_at: chrono::Utc::now(),
            meter_start_wh: 0,
        });
        snap
    }

    #[test]
    fn commands_read_as_sentences() {
        let snap = Snapshot::initial(&StationConfig::new("ws://h/p"));
        let c = ConnectorId::new(1);
        let text = |command| done_text(&command, &snap).unwrap();
        assert_eq!(
            text(Command::PlugIn { connector: c }),
            "Plugged in connector 1"
        );
        assert_eq!(
            text(Command::MeterValues {
                connector: c,
                wh_delta: 120,
                measurands: None
            }),
            "Sent meter values (+120 Wh)"
        );
        assert_eq!(
            text(Command::Fault {
                connector: c,
                error_code: ChargePointErrorCode::ConnectorLockFailure,
                info: None,
                vendor_error_code: None
            }),
            "Fault ConnectorLockFailure on connector 1"
        );
        assert!(done_text(&Command::Shutdown, &snap).is_none());
    }

    #[test]
    fn a_fault_says_when_it_ends_the_transaction() {
        let snap = snapshot_with_tx();
        let text = done_text(
            &Command::Fault {
                connector: ConnectorId::new(1),
                error_code: ChargePointErrorCode::ConnectorLockFailure,
                info: None,
                vendor_error_code: None,
            },
            &snap,
        )
        .unwrap();
        assert_eq!(
            text,
            "Fault ConnectorLockFailure on connector 1. Transaction 7 ended by the fault"
        );
        let stop = done_text(
            &Command::StopTransaction {
                connector: ConnectorId::new(1),
                reason: StopReason::Local,
                id_tag: None,
            },
            &snap,
        )
        .unwrap();
        assert_eq!(stop, "Stopped the transaction on connector 1 (Local)");
    }
}
