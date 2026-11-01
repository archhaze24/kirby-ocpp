//! Outbound CALLs of the station actor.
//!
//! # Design: spawned future + continuation
//!
//! The actor never awaits a CSMS round trip. [`StationActor::call_raw`] spawns a task
//! that awaits `Connection::call` and reports `Internal::CallDone{token, ..}` back
//! through the actor's internal channel. The actor keeps the *continuation* for that
//! token in a map: a boxed `FnOnce(&mut StationActor, Result<Value, CallError>)`. When
//! `CallDone` arrives the actor looks the continuation up and runs it **inside the
//! actor loop**, so it may mutate any state and answer a waiting command without locks.
//! Continuations capture what they need (a command's `oneshot` reply, a connector id).
//!
//! * Exactly-once: a continuation is removed from the map before it runs; it runs
//!   either from `CallDone` or from [`StationActor::fail_pending_calls`] (connection lost,
//!   disconnect, shutdown), never both.
//! * The connection serializes CALLs on the wire, so the continuations of two calls run
//!   in the order the calls were sent.
//! * **Retries** for `StartTransaction` / `StopTransaction` / `MeterValues`
//!   ([`is_transaction_message`]): the spawned task loops
//!   `TransactionMessageAttempts` times with `TransactionMessageRetryInterval` between
//!   attempts (values read when the call is made). CALLERROR, timeout and schema violations
//!   are retried, a closed connection is not. The task is aborted when the connection
//!   goes away and the continuation then gets `Closed`, so retries never outlive a link.
//! * **Gating** ([`Origin`]): until `BootNotification` was `Accepted` only `BootNotification`
//!   may be sent. While `Pending`, messages the CSMS asked for with `TriggerMessage`
//!   ([`Origin::Triggered`]) are allowed as well (OCPP 1.6 section 4.2). [`Origin::Raw`]
//!   (the `SendRaw` command, for negative testing) bypasses the gate.

use std::time::Duration;

use serde_json::Value;
use tokio::sync::oneshot;

use super::actor::{Internal, StationActor};
use super::command::Outcome;
use crate::error::{CallError, StationError};
use crate::ocpp::action::OutboundAction;
use crate::ocpp::types::RegistrationStatus;
use crate::station::event::LogLevel;

/// Answer channel of a command.
pub(super) type Reply = oneshot::Sender<Result<Outcome, StationError>>;

/// Runs once, inside the actor, when a CALL has been answered or failed.
pub(super) type Continuation = Box<dyn FnOnce(&mut StationActor, Result<Value, CallError>) + Send>;

/// Why a message is being sent; decides whether the registration gate lets it through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Spontaneous or user-initiated.
    Station,
    /// Asked for by the CSMS with `TriggerMessage`.
    Triggered,
    /// `SendRaw`: no gating, no retries.
    Raw,
}

/// A CALL that has been handed to the connection and not completed yet.
pub(super) struct PendingCall {
    pub(super) action: String,
    pub(super) task: tokio::task::JoinHandle<()>,
    pub(super) continuation: Continuation,
}

/// The actions that are retried (TS `isTransactionMessage`).
pub fn is_transaction_message(action: &str) -> bool {
    matches!(
        action,
        "StartTransaction" | "StopTransaction" | "MeterValues"
    )
}

/// `true` if a failed attempt of a transaction message should be tried again.
fn retryable(error: &CallError) -> bool {
    !matches!(
        error,
        CallError::NotConnected | CallError::Closed { .. } | CallError::NotRegistered { .. }
    )
}

impl StationActor {
    /// Whether a message `action` of `origin` may be sent in the current registration state.
    pub(super) fn may_send(&self, action: &str, origin: Origin) -> Result<(), CallError> {
        let allowed = match (origin, self.state.registration) {
            (Origin::Raw, _) => true,
            (_, _) if action == "BootNotification" => true,
            (_, Some(RegistrationStatus::Accepted))
            | (Origin::Triggered, Some(RegistrationStatus::Pending)) => true,
            _ => false,
        };
        if allowed {
            return Ok(());
        }
        Err(CallError::NotRegistered {
            action: action.to_owned(),
            state: self
                .state
                .registration
                .map_or_else(|| "unregistered".to_owned(), |s| s.as_str().to_owned()),
        })
    }

    /// Sends a CALL and runs `continuation` with its outcome. Never blocks. If the
    /// message cannot be sent at all, the continuation runs immediately with the error.
    pub(super) fn call_raw(
        &mut self,
        origin: Origin,
        action: &str,
        payload: Value,
        validate: bool,
        continuation: Continuation,
    ) {
        let Some(connection) = self.link.conn.clone() else {
            continuation(self, Err(CallError::NotConnected));
            return;
        };
        if let Err(error) = self.may_send(action, origin) {
            continuation(self, Err(error));
            return;
        }

        let token = self.next_call_token;
        self.next_call_token += 1;
        let generation = self.link.generation;
        let (attempts, interval) = if origin != Origin::Raw && is_transaction_message(action) {
            (
                self.registry.transaction_message_attempts(),
                self.registry.transaction_message_retry_interval(),
            )
        } else {
            (1, Duration::ZERO)
        };
        let timeout = self.config.call_timeout();
        let results = self.internal_tx.clone();
        let name = action.to_owned();

        let task = tokio::spawn({
            let name = name.clone();
            async move {
                let mut attempt = 1;
                let result = loop {
                    let outcome = if validate {
                        connection.call(&name, payload.clone(), Some(timeout)).await
                    } else {
                        connection
                            .call_unvalidated(&name, payload.clone(), Some(timeout))
                            .await
                    };
                    match outcome {
                        Err(error) if attempt < attempts && retryable(&error) => {
                            let _ = results.send(Internal::Log(
                                LogLevel::Warn,
                                format!(
                                    "{name} attempt {attempt}/{attempts} failed: {error}; retrying in {}s",
                                    interval.as_secs()
                                ),
                            ));
                            tokio::time::sleep(interval).await;
                            attempt += 1;
                        }
                        other => break other,
                    }
                };
                let _ = results.send(Internal::CallDone {
                    token,
                    generation,
                    result,
                });
            }
        });
        self.calls.insert(
            token,
            PendingCall {
                action: name,
                task,
                continuation,
            },
        );
    }

    /// Typed variant of [`Self::call_raw`] for charge-point-initiated actions.
    pub(super) fn call<A>(
        &mut self,
        origin: Origin,
        request: &A::Req,
        continuation: impl FnOnce(&mut StationActor, Result<A::Res, CallError>) + Send + 'static,
    ) where
        A: OutboundAction,
    {
        let payload = match serde_json::to_value(request) {
            Ok(payload) => payload,
            Err(source) => {
                continuation(
                    self,
                    Err(CallError::Serialize {
                        action: A::NAME.to_owned(),
                        source,
                    }),
                );
                return;
            }
        };
        self.call_raw(
            origin,
            A::NAME,
            payload,
            true,
            Box::new(move |actor, result| {
                let typed = result.and_then(|value| {
                    serde_json::from_value::<A::Res>(value).map_err(|source| CallError::Decode {
                        action: A::NAME.to_owned(),
                        source,
                    })
                });
                continuation(actor, typed);
            }),
        );
    }

    /// `CallDone` arrived from a call task.
    pub(super) fn on_call_done(
        &mut self,
        token: u64,
        generation: u64,
        result: Result<Value, CallError>,
    ) {
        if generation != self.link.generation {
            return;
        }
        let Some(call) = self.calls.remove(&token) else {
            return;
        };
        match &result {
            Ok(_) => self.log(LogLevel::Success, format!("{} accepted", call.action)),
            Err(error) => self.log(LogLevel::Error, error.to_string()),
        }
        (call.continuation)(self, result);
    }

    /// Fails every call in flight (connection lost, disconnect, shutdown): the tasks (and
    /// their retry sleeps) are aborted and each continuation runs once with `error()`.
    pub(super) fn fail_pending_calls(&mut self, error: impl Fn() -> CallError) {
        let pending = std::mem::take(&mut self.calls);
        let mut ordered: Vec<_> = pending.into_iter().collect();
        ordered.sort_by_key(|(token, _)| *token);
        for (_, call) in ordered {
            call.task.abort();
            (call.continuation)(self, Err(error()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_transaction_messages_are_retried() {
        for action in ["StartTransaction", "StopTransaction", "MeterValues"] {
            assert!(is_transaction_message(action));
        }
        for action in [
            "Authorize",
            "StatusNotification",
            "Heartbeat",
            "BootNotification",
        ] {
            assert!(!is_transaction_message(action));
        }
    }

    #[test]
    fn closed_connections_are_not_retried() {
        assert!(!retryable(&CallError::Closed { code: None }));
        assert!(!retryable(&CallError::NotConnected));
        assert!(retryable(&CallError::Timeout {
            action: "StartTransaction".into(),
            after: Duration::from_secs(1),
        }));
    }
}
