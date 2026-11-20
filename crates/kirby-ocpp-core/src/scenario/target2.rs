//! The OCPP 2.0.1 and 2.1 scenario target: runs scenario steps against a
//! [`Station2Handle`](crate::station2::Station2Handle).
//!
//! The runner handles `sleep`, `expect_call`, variables and reports itself. This module maps
//! the remaining steps to [`Command2`]s, checks each outcome against the step's `expect`,
//! answers `wait_for` from the station snapshot and arms scripted replies.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::time::timeout;

use super::model::{Action, Expect, Step};
use super::model2::{Action2, Condition2};
use super::runner::{Evidence, ScenarioTarget, WaitFailure, json_subset, scripted_reply, sealed};
use super::vars;
use crate::error::StationError;
use crate::ocpp::Protocol;
use crate::ocpp::schema::SchemaRegistry;
use crate::ocpp::v2::registry::actions_for;
use crate::ocpp::v2::types::{Component, Evse};
use crate::station::Event;
use crate::station2::{Command2, Outcome2, Snapshot2, Station2Handle};
use crate::ws::ConnectionState;

impl Condition2 {
    /// Does `snapshot` satisfy the condition?
    pub(super) fn holds(&self, snapshot: &Snapshot2) -> bool {
        let transaction = |evse: u32| snapshot.evse(evse).map(|evse| evse.transaction.as_ref());
        match self {
            Self::Connected {} => snapshot.connection == ConnectionState::Connected,
            Self::Disconnected {} => snapshot.connection == ConnectionState::Disconnected,
            Self::Registration { status } => snapshot.registration == Some(*status),
            Self::ConnectorStatus {
                evse,
                connector,
                status,
            } => snapshot
                .evse(*evse)
                .and_then(|e| e.connectors.iter().find(|c| c.id == *connector))
                .is_some_and(|c| c.status == *status),
            Self::TxActive { evse } => transaction(*evse).is_some_and(|tx| tx.is_some()),
            Self::TxIdle { evse } => transaction(*evse).is_some_and(|tx| tx.is_none()),
            Self::ChargingState { evse, state } => transaction(*evse)
                .flatten()
                .is_some_and(|tx| tx.charging_state == *state),
            Self::NoPendingMessages {} => snapshot.queued_events == 0,
        }
    }
}

impl sealed::Sealed for Station2Handle {}

impl ScenarioTarget for Station2Handle {
    fn protocol(&self) -> Protocol {
        station_protocol(self)
    }

    fn station_id(&self) -> String {
        self.snapshot().charge_point_id
    }

    fn subscribe(&self) -> broadcast::Receiver<Event> {
        Station2Handle::subscribe(self)
    }

    async fn wait_for(&self, step: &Step) -> Result<(), WaitFailure> {
        let Action::V201(action) = &step.action else {
            return Err(WaitFailure {
                reason: not_for_2_x(step, station_protocol(self)),
                expected: None,
            });
        };
        let Action2::WaitFor {
            condition,
            timeout_ms,
        } = &**action
        else {
            return Err(WaitFailure {
                reason: format!("`{}` is not a wait_for step", action.name()),
                expected: None,
            });
        };
        let mut rx = self.watch_snapshot();
        match timeout(
            Duration::from_millis(*timeout_ms),
            rx.wait_for(|s| condition.holds(s)),
        )
        .await
        {
            Ok(Ok(snapshot)) => {
                drop(snapshot);
                Ok(())
            }
            Ok(Err(_)) => Err(WaitFailure {
                reason: "station stopped".to_owned(),
                expected: None,
            }),
            Err(_) => Err(WaitFailure {
                reason: format!("timed out after {timeout_ms} ms waiting for {condition:?}"),
                expected: serde_json::to_value(condition).ok(),
            }),
        }
    }

    async fn run_command_step(
        &self,
        step: &Step,
        tag: u64,
        evidence: &mut Evidence,
    ) -> Result<Option<Value>, String> {
        if let Action::Reply {
            call,
            result,
            error,
            times,
            delay_ms,
            ..
        } = &step.action
        {
            let protocol = station_protocol(self);
            let action = canonical_action(protocol, call)?;
            let registry = SchemaRegistry::for_protocol(protocol).ok();
            let reply = scripted_reply(
                tag,
                action,
                result.as_ref(),
                error.as_ref(),
                *times,
                *delay_ms,
                registry.as_deref(),
            )?;
            self.send(Command2::ArmScriptedReply { reply })
                .await
                .map_err(|error| format!("reply: {error}"))?;
            return Ok(None);
        }
        let commands = to_commands(step, station_protocol(self))?;
        // Commands run in order; the first failure decides the step. The outcome of the last
        // one is the step's.
        let mut result = Ok(Outcome2::Done);
        for command in commands {
            // Outside a transaction `AdvanceMeter` already sent the reading; a sample would
            // repeat it. The snapshot is read after `AdvanceMeter` was answered: the actor
            // publishes after replying, so a snapshot read earlier can miss a transaction the
            // previous step started.
            if let Command2::SampleMeter { evse } = &command
                && self
                    .snapshot()
                    .evse(*evse)
                    .is_some_and(|evse| evse.transaction.is_none())
            {
                continue;
            }
            result = self.send(command).await;
            if result.is_err() {
                break;
            }
        }
        check(step, result, evidence)
    }

    async fn clear_replies(&self) -> Option<Value> {
        let cleared = timeout(
            Duration::from_secs(2),
            self.send(Command2::ClearScriptedReplies),
        )
        .await;
        match cleared {
            Ok(Ok(Outcome2::Response { payload })) => Some(payload),
            _ => None,
        }
    }
}

/// The OCPP version of `station`: 2.1 or 2.0.1.
fn station_protocol(station: &Station2Handle) -> Protocol {
    if station.config().protocol == Protocol::V21 {
        Protocol::V21
    } else {
        Protocol::V201
    }
}

/// Failure reason of a step that is written for another protocol.
fn not_for_2_x(step: &Step, protocol: Protocol) -> String {
    format!(
        "`{}` is an OCPP 1.6 step; this station speaks OCPP {}",
        step.action.name(),
        protocol.ocpp_version()
    )
}

/// Canonical spelling of a CSMS command of `protocol` (case-insensitive), for a `reply` step.
fn canonical_action(protocol: Protocol, name: &str) -> Result<&'static str, String> {
    actions_for(protocol)
        .iter()
        .find(|info| info.direction.csms_may_send() && info.name.eq_ignore_ascii_case(name.trim()))
        .map(|info| info.name)
        .ok_or_else(|| {
            format!(
                "reply: unknown action `{}` (expected one of the CSMS commands, e.g. Reset)",
                vars::clip_text(name.trim())
            )
        })
}

fn evse_ref(id: u32) -> Result<Evse, String> {
    Ok(Evse {
        id: i32::try_from(id).map_err(|_| format!("`evse` {id} is out of range"))?,
        connector_id: None,
        custom_data: None,
    })
}

/// The commands a step sends, in order (variables already substituted).
fn to_commands(step: &Step, protocol: Protocol) -> Result<Vec<Command2>, String> {
    let action = match &step.action {
        Action::Connect {} => Command2::Connect,
        Action::Disconnect {} => Command2::Disconnect,
        Action::Reconnect {} => Command2::Reconnect,
        Action::Boot {} => Command2::Boot,
        Action::Heartbeat {} => Command2::Heartbeat,
        Action::SendRaw {
            call,
            payload,
            validate,
        } => Command2::SendRaw {
            action: call.clone(),
            payload: payload.clone(),
            validate: *validate,
        },
        Action::SetRule { .. } | Action::ClearRules { .. } | Action::RequestCertificate {} => {
            return Err(format!(
                "`{}` is not available for OCPP {}",
                step.action.name(),
                protocol.ocpp_version()
            ));
        }
        Action::V201(action) => return v201_commands(action),
        Action::Sleep { .. }
        | Action::WaitFor { .. }
        | Action::ExpectCall { .. }
        | Action::Reply { .. }
        | Action::Repeat { .. } => return Err("not a command".to_owned()),
        _ => return Err(not_for_2_x(step, protocol)),
    };
    Ok(vec![action])
}

fn v201_commands(action: &Action2) -> Result<Vec<Command2>, String> {
    Ok(match action.clone() {
        Action2::Plug { evse } => vec![Command2::PlugIn { evse }],
        Action2::Unplug { evse } => vec![Command2::Unplug { evse }],
        Action2::Authorize {
            id_token,
            token_type,
        } => vec![Command2::Authorize {
            id_token,
            token_type,
        }],
        Action2::StartTx {
            evse,
            id_token,
            token_type,
        } => vec![Command2::PresentToken {
            evse: Some(evse),
            id_token,
            token_type,
        }],
        Action2::PresentToken {
            evse,
            id_token,
            token_type,
        } => vec![Command2::PresentToken {
            evse,
            id_token,
            token_type,
        }],
        Action2::StopTx { evse, reason } => vec![Command2::StopTransaction { evse, reason }],
        Action2::Meter { evse, wh_delta } => vec![
            Command2::AdvanceMeter { evse, wh_delta },
            Command2::SampleMeter { evse },
        ],
        Action2::ChargingState { evse, state } => {
            vec![Command2::SetChargingState { evse, state }]
        }
        Action2::Fault { evse, connector } => vec![Command2::Fault { evse, connector }],
        Action2::ClearFault { evse, connector } => {
            vec![Command2::ClearFault { evse, connector }]
        }
        Action2::StatusNotification { evse, connector } => {
            vec![Command2::StatusNotification { evse, connector }]
        }
        Action2::SetVariable {
            component,
            variable,
            value,
        } => {
            let evse = component.evse.map(evse_ref).transpose()?;
            vec![Command2::SetVariable {
                component: Component {
                    evse,
                    name: component.name,
                    instance: component.instance,
                    custom_data: None,
                },
                variable: crate::ocpp::v2::types::Variable {
                    name: variable.name,
                    instance: variable.instance,
                    custom_data: None,
                },
                value,
            }]
        }
        Action2::SecurityEvent {
            event_type,
            tech_info,
        } => vec![Command2::SendSecurityEvent {
            event_type,
            tech_info,
        }],
        Action2::DataTransfer {
            vendor_id,
            message_id,
            data,
        } => vec![Command2::DataTransfer {
            vendor_id,
            message_id,
            data,
        }],
        Action2::WaitFor { .. } => return Err("not a command".to_owned()),
    })
}

/// The `status` an outcome carries, for `expect.status`.
fn outcome_status(outcome: &Outcome2) -> Option<String> {
    match outcome {
        Outcome2::Authorized { status, .. } => Some(status.clone()),
        Outcome2::TransactionStarted { .. } => Some("Accepted".to_owned()),
        Outcome2::Response { payload } => payload
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

/// What a `capture` can read from the outcome of a step: the CALLRESULT payload, or for the
/// token steps (`authorize`, `start_tx`, `present_token`) a synthesized answer with the keys
/// `status`, `transaction_id` and `evse` (null when unknown).
fn answer(action: &Action, outcome: Outcome2) -> Option<Value> {
    let requested = match action {
        Action::V201(action) => match &**action {
            Action2::Authorize { .. } => Some(None),
            Action2::StartTx { evse, .. } => Some(Some(*evse)),
            Action2::PresentToken { evse, .. } => Some(*evse),
            _ => None,
        },
        _ => None,
    };
    match (outcome, requested) {
        (Outcome2::Response { payload }, _) => Some(payload),
        (
            Outcome2::TransactionStarted {
                evse,
                transaction_id,
            },
            Some(_),
        ) => Some(json!({
            "status": "Accepted",
            "transaction_id": transaction_id,
            "evse": evse,
        })),
        (Outcome2::Authorized { status, .. }, Some(evse)) => Some(json!({
            "status": status,
            "transaction_id": null,
            "evse": evse,
        })),
        _ => None,
    }
}

/// Checks the outcome of a step against its `expect`; returns what a `capture` may read.
fn check(
    step: &Step,
    result: Result<Outcome2, StationError>,
    evidence: &mut Evidence,
) -> Result<Option<Value>, String> {
    let name = step.action.name();
    let default = Expect::default();
    let expect = step.expect.as_ref().unwrap_or(&default);
    let outcome = match result {
        Err(_) if expect.error => return Ok(None),
        Err(error) => return Err(format!("{name}: {error}")),
        Ok(_) if expect.error => {
            return Err(format!("{name}: expected an error, but it succeeded"));
        }
        Ok(outcome) => outcome,
    };
    let (implicit, starts_transaction) = match &step.action {
        Action::V201(action) => (
            matches!(
                **action,
                Action2::Authorize { .. } | Action2::StartTx { .. }
            ),
            matches!(**action, Action2::StartTx { .. }),
        ),
        _ => (false, false),
    };
    let wanted = match (&expect.status, implicit) {
        (Some(status), _) => Some(status.as_str()),
        (None, true) => Some("Accepted"),
        (None, false) => None,
    };
    if starts_transaction && wanted == Some("Accepted") {
        // An accepted token starts a transaction only once the start point is reached; a token
        // that ends a running authorization starts none either.
        let actual = match &outcome {
            Outcome2::Authorized { status, .. } if status == "Accepted" => Some("authorized"),
            Outcome2::Done => Some("done"),
            _ => None,
        };
        if let Some(actual) = actual {
            evidence.expected = Some(json!({"outcome": "transaction_started"}));
            evidence.actual = Some(actual.to_owned());
            return Err(format!(
                "{name}: the token was accepted, but no transaction started"
            ));
        }
    }
    if let Some(wanted) = wanted {
        match outcome_status(&outcome) {
            Some(actual) if actual == wanted => {}
            Some(actual) => {
                evidence.expected = Some(Value::String(wanted.to_owned()));
                evidence.actual = Some(vars::clip_text(&actual));
                return Err(format!(
                    "{name}: expected status {wanted}, got {}",
                    vars::clip_text(&actual)
                ));
            }
            None => {
                evidence.expected = Some(Value::String(wanted.to_owned()));
                return Err(format!(
                    "{name}: expected status {wanted}, but the outcome carries none"
                ));
            }
        }
    }
    if let Some(wanted) = &expect.payload {
        match &outcome {
            Outcome2::Response { payload } if json_subset(wanted, payload) => {}
            Outcome2::Response { payload } => {
                let (wanted, payload) = (super::redacted(wanted), super::redacted(payload));
                evidence.expected = Some(wanted.clone());
                evidence.actual = Some(vars::clip_json(&payload));
                return Err(format!(
                    "{name}: response {} does not contain {}",
                    vars::clip_json(&payload),
                    vars::clip_json(&wanted)
                ));
            }
            _ => {
                return Err(format!(
                    "{name}: expected a response payload, the outcome has none"
                ));
            }
        }
    }
    Ok(answer(&step.action, outcome))
}
