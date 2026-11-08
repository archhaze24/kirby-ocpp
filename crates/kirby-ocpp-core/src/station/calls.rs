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
//!   attempts (values read when the call is made). A retry is queued behind CALLs made
//!   since the first attempt, so retries may reorder; only a `MeterValues` retry is dropped
//!   once the `StopTransaction` of its transaction was queued ([`RetryGuard`]). CALLERROR, timeout and schema violations
//!   are retried, a closed connection is not. The task is aborted when the connection
//!   goes away and the continuation then gets `Closed`, so retries never outlive a link.
//! * **Gating** ([`Origin`]): until `BootNotification` was `Accepted` only `BootNotification`
//!   may be sent. While `Pending`, messages the CSMS asked for with `TriggerMessage`
//!   ([`Origin::Triggered`]) are allowed as well (OCPP 1.6 section 4.2). [`Origin::Raw`]
//!   (the `SendRaw` command, for negative testing) bypasses the gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use super::actor::{Internal, StationActor};
use super::command::Outcome;
use crate::error::{CallError, StationError};
use crate::ocpp::action::OutboundAction;
use crate::ocpp::types::RegistrationStatus;
use crate::station::event::LogLevel;
use crate::ws::{Connection, PendingReply};

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
    pub(super) origin: Origin,
    pub(super) task: tokio::task::JoinHandle<()>,
    pub(super) continuation: Continuation,
    /// `MeterValues` with retries: set once the transaction's `StopTransaction` was queued.
    pub(super) retry_guard: Option<RetryGuard>,
}

/// Lets the actor stop the retries of a transaction's `MeterValues` once its stop is sent.
/// The retry task holds the lock while it checks the flag and queues the retry, so a retry
/// is either queued before the `StopTransaction` or not at all.
#[derive(Clone)]
pub(super) struct RetryGuard {
    tx: i32,
    stopped: Arc<Mutex<bool>>,
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

/// Retry plan of one CALL.
struct Attempts {
    connection: Connection,
    name: String,
    payload: Value,
    timeout: Duration,
    validate: bool,
    tries: u32,
    interval: Duration,
    guard: Option<RetryGuard>,
    log: mpsc::UnboundedSender<Internal>,
}

impl Attempts {
    /// Waits for `first`, then retries retryable failures. A request that cannot be queued
    /// (schema, serialisation, closed link) would fail the same way again: no retry.
    async fn run(self, first: Result<PendingReply, CallError>) -> Result<Value, CallError> {
        let Self {
            connection,
            name,
            payload,
            timeout,
            validate,
            tries: attempts,
            interval,
            guard,
            log,
        } = self;
        let name = name.as_str();
        let mut attempt = 1;
        let mut queued = first;
        loop {
            let outcome = queued?.wait().await;
            match outcome {
                Err(error) if attempt < attempts && retryable(&error) => {
                    let _ = log.send(Internal::Log(
                        LogLevel::Warn,
                        format!(
                            "{name} attempt {attempt}/{attempts} failed: {error}; retrying in {}s",
                            interval.as_secs()
                        ),
                    ));
                    tokio::time::sleep(interval).await;
                    attempt += 1;
                    let enqueue =
                        || connection.enqueue(name, payload.clone(), Some(timeout), validate);
                    let retry = match &guard {
                        Some(guard) => {
                            let stopped = guard
                                .stopped
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if *stopped { None } else { Some(enqueue()) }
                        }
                        None => Some(enqueue()),
                    };
                    let Some(retry) = retry else {
                        let _ = log.send(Internal::Log(
                            LogLevel::Info,
                            format!("{name} retry dropped: the transaction was already stopped"),
                        ));
                        return Err(error);
                    };
                    queued = retry;
                }
                other => return other,
            }
        }
    }
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

        if action == "StopTransaction" {
            // A retried sample must not overtake the stop of its transaction.
            if let Some(tx) = payload.get("transactionId").and_then(Value::as_i64) {
                for call in self.calls.values() {
                    if let Some(guard) = call.retry_guard.as_ref().filter(|g| i64::from(g.tx) == tx)
                    {
                        *guard
                            .stopped
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
                    }
                }
            }
        }
        let retry_guard = (attempts > 1 && action == "MeterValues")
            .then(|| {
                let tx = payload.get("transactionId").and_then(Value::as_i64)?;
                Some(RetryGuard {
                    tx: i32::try_from(tx).ok()?,
                    stopped: Arc::new(Mutex::new(false)),
                })
            })
            .flatten();
        // The first attempt is queued on the connection right here, in call order: spawned
        // tasks are polled in scheduler order, which would reorder CALLs made back to back
        // (offline samples followed by their StopTransaction).
        let first = connection.enqueue(&name, payload.clone(), Some(timeout), validate);
        let task = tokio::spawn({
            let name = name.clone();
            let guard = retry_guard.clone();
            async move {
                let plan = Attempts {
                    connection,
                    name,
                    payload,
                    timeout,
                    validate,
                    tries: attempts,
                    interval,
                    guard,
                    log: results.clone(),
                };
                let result = plan.run(first).await;
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
                origin,
                task,
                continuation,
                retry_guard,
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
            // The station's own BootNotification handler logs the outcome itself (accepted
            // with the heartbeat and clock details, or pending/rejected): a generic line
            // would repeat it, and would read "accepted" for a rejected boot. A raw or
            // triggered one has no such handler line and keeps the generic one.
            Ok(_) if call.action == "BootNotification" && call.origin == Origin::Station => {}
            Ok(_) => self.log(LogLevel::Success, format!("{} accepted", call.action)),
            Err(error) => self.log(LogLevel::Error, error.to_string()),
        }
        (call.continuation)(self, result);
    }

    /// Stops waiting for the oldest unanswered CALL of `action` (`CancelCall`). The call is
    /// forgotten: its continuation runs once with [`CallError::Cancelled`], and the answer, if
    /// one still arrives, is logged as a frame and dropped (`on_call_done` ignores unknown
    /// tokens). The CALL itself cannot be recalled from the wire.
    ///
    /// Transaction messages are refused: the CSMS may already have acted on them, and the
    /// station state has to follow its answer or the retry logic.
    pub(super) fn cancel_call(&mut self, action: &str) -> Result<(), StationError> {
        if is_transaction_message(action) {
            return Err(StationError::InvalidState {
                connector: None,
                reason: format!("{action} cannot be cancelled: the transaction follows its answer"),
            });
        }
        let token = self
            .calls
            .iter()
            .filter(|(_, call)| call.action == action)
            .map(|(token, _)| *token)
            .min()
            .ok_or_else(|| StationError::InvalidState {
                connector: None,
                reason: format!("no unanswered {action} call to cancel"),
            })?;
        if let Some(call) = self.calls.remove(&token) {
            call.task.abort();
            self.log(LogLevel::Warn, format!("{action} cancelled"));
            (call.continuation)(
                self,
                Err(CallError::Cancelled {
                    action: action.to_owned(),
                }),
            );
        }
        Ok(())
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
