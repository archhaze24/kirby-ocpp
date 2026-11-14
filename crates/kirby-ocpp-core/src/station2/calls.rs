//! Outbound CALLs of the station actor.
//!
//! The actor never awaits a CSMS round trip: a CALL stores a continuation that runs inside
//! the actor loop when the answer arrives, the CALL fails or the link goes away. Whether a
//! message may leave at all depends on the registration state and on why it is sent
//! ([`Origin2`]).
//!
//! # Design: reply future + continuation
//!
//! [`Station2Actor::call_raw`] queues the CALL on the connection and stores a future that
//! polls the reply channel and returns `Internal2::CallDone{token, ..}`. The actor polls
//! those futures next to its other event sources and keeps the continuation of each token in
//! a map. A continuation is removed from the map before it runs; it runs either from
//! `CallDone` or from [`Station2Actor::fail_pending_calls`] (link lost, disconnect, shutdown),
//! never both. The connection serializes CALLs on the wire, so continuations of two calls run
//! in the order the calls were sent.
//!
//! Unlike the 1.6 station there is no retry here: `TransactionEvent` retries (E13) live in the
//! transaction queue, every other CALL is tried once.
//!
//! # Gating
//!
//! Until `BootNotification` was `Accepted` only `BootNotification` may be sent. While
//! `Pending`, messages the CSMS asked for ([`Origin2::Triggered`]) are allowed as well
//! (B02.FR.02). [`Origin2::Raw`] (the `SendRaw` command, for negative testing) bypasses the
//! gate.

use std::collections::HashMap;

use futures_util::FutureExt;
use futures_util::future::{AbortHandle, Abortable, BoxFuture};
use futures_util::stream::FuturesUnordered;
use serde_json::Value;

use super::actor::{Internal2, Station2Actor};
use super::command::Outcome2;
use super::types::{Continuation2, Origin2, Reply2};
use crate::error::{CallError, StationError};
use crate::ocpp::action::OutboundAction;
use crate::ocpp::v2::messages::{
    RegistrationStatus, SecurityEventNotification, SecurityEventNotificationRequest,
};
use crate::ocpp::v2::types::WireEnum;
use crate::station::LogLevel;

/// How a failed CALL is reported to the caller of a command.
pub(super) fn call_failure(error: CallError) -> StationError {
    match error {
        CallError::NotConnected => StationError::NotConnected,
        other => StationError::Call(other),
    }
}

/// A CALL that has been handed to the connection and not completed yet.
pub(super) struct PendingCall {
    action: String,
    origin: Origin2,
    task: AbortHandle,
    continuation: Continuation2,
}

/// CALLs handed to the connection and not completed yet.
#[derive(Default)]
pub(crate) struct Calls2 {
    /// Continuations by call token.
    pub(super) pending: HashMap<u64, PendingCall>,
    /// The reply futures; polled by the actor loop.
    pub(super) results: FuturesUnordered<BoxFuture<'static, Internal2>>,
    next_token: u64,
}

impl std::fmt::Debug for Calls2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Calls2")
            .field("pending", &self.pending.len())
            .field("results", &self.results.len())
            .finish_non_exhaustive()
    }
}

impl Station2Actor {
    /// Sends a CALL with a JSON payload; `cont` runs when it completed. Never blocks. If the
    /// message cannot be sent at all, the continuation runs immediately with the error.
    pub(super) fn call_raw(
        &mut self,
        origin: Origin2,
        action: &str,
        payload: Value,
        validate: bool,
        cont: Continuation2,
    ) {
        let Some(connection) = self.link.conn.clone() else {
            cont(self, Err(CallError::NotConnected));
            return;
        };
        if let Err(error) = self.may_send(action, origin) {
            cont(self, Err(error));
            return;
        }

        let token = self.calls.next_token;
        self.calls.next_token += 1;
        let generation = self.link.generation;
        let timeout = self.config.call_timeout();
        // Queue the CALL synchronously, in call order: the order in which reply futures are
        // polled must not reorder what goes on the wire.
        let queued = connection.enqueue(action, payload, Some(timeout), validate);
        let (task, registration) = AbortHandle::new_pair();
        self.calls.results.push(Box::pin(async move {
            let result = Abortable::new(async move { queued?.wait().await }, registration)
                .await
                .unwrap_or(Err(CallError::Closed { code: None }));
            Internal2::CallDone {
                token,
                generation,
                result,
            }
        }));
        self.calls.pending.insert(
            token,
            PendingCall {
                action: action.to_owned(),
                origin,
                task,
                continuation: cont,
            },
        );
    }

    /// Sends the typed CALL of `A`; `cont` runs when it completed.
    pub(super) fn call<A: OutboundAction>(
        &mut self,
        origin: Origin2,
        req: &A::Req,
        cont: impl FnOnce(&mut Self, Result<A::Res, CallError>) + Send + 'static,
    ) {
        let payload = match serde_json::to_value(req) {
            Ok(payload) => payload,
            Err(source) => {
                cont(
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
                cont(actor, typed);
            }),
        );
    }

    /// The registration gate: whether `action` may be sent now.
    pub(super) fn may_send(&self, action: &str, origin: Origin2) -> Result<(), CallError> {
        let allowed = match (origin, self.state.registration) {
            (Origin2::Raw, _) => true,
            (_, _) if action == "BootNotification" => true,
            (_, Some(RegistrationStatus::Accepted))
            | (Origin2::Triggered, Some(RegistrationStatus::Pending)) => true,
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

    /// `CallDone` arrived from a reply future.
    pub(super) fn on_call_done(
        &mut self,
        token: u64,
        generation: u64,
        result: Result<Value, CallError>,
    ) {
        if generation != self.link.generation {
            return;
        }
        let Some(call) = self.calls.pending.remove(&token) else {
            return;
        };
        match &result {
            // The boot handler logs the outcome itself (accepted with the heartbeat and clock
            // details, or pending/rejected): a generic line would repeat it, and would read
            // "accepted" for a rejected boot. A raw or triggered boot has no such line and
            // keeps the generic one.
            Ok(_) if call.action == "BootNotification" && call.origin == Origin2::Station => {}
            Ok(_) => self.log(LogLevel::Success, format!("{} accepted", call.action)),
            Err(error) => self.log(LogLevel::Error, error.to_string()),
        }
        (call.continuation)(self, result);
    }

    /// Fails every call in flight (link lost, disconnect, shutdown): the futures are aborted
    /// and each continuation runs once with `error()`.
    pub(super) fn fail_pending_calls(&mut self, error: impl Fn() -> CallError) {
        let pending = std::mem::take(&mut self.calls.pending);
        let mut ordered: Vec<_> = pending.into_iter().collect();
        ordered.sort_by_key(|(token, _)| *token);
        for (_, call) in ordered {
            call.task.abort();
            (call.continuation)(self, Err(error()));
        }
    }

    /// The connection resolves all reply channels before it emits `Closed`. Polls them
    /// locally before the link is torn down, including futures not polled previously.
    pub(super) fn finish_answered_calls(&mut self) {
        let mut ready = Vec::new();
        // Poll each future exactly once, bypassing both stream fairness and Tokio's
        // cooperative budget: a ready reply must not be mistaken for a pending one.
        for future in std::mem::take(&mut self.calls.results) {
            if let Some(Internal2::CallDone {
                token,
                generation,
                result,
            }) = tokio::task::unconstrained(future).now_or_never()
            {
                ready.push((token, generation, result));
            }
        }
        ready.sort_by_key(|(token, _, _)| *token);
        for (token, generation, result) in ready {
            self.on_call_done(token, generation, result);
        }
    }

    /// `Command2::SendRaw`.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the command router passes the command's fields by value"
    )]
    pub(super) fn cmd_send_raw(
        &mut self,
        action: String,
        payload: Value,
        validate: bool,
        reply: Reply2,
    ) {
        self.call_raw(
            Origin2::Raw,
            &action,
            payload,
            validate,
            Box::new(move |_, result| {
                let answer = result
                    .map(|payload| Outcome2::Response { payload })
                    .map_err(call_failure);
                // The requester may have gone away; nothing to do about that.
                let _ = reply.send(answer);
            }),
        );
    }

    /// `Command2::SendSecurityEvent`.
    pub(super) fn cmd_send_security_event(
        &mut self,
        event_type: String,
        tech_info: Option<String>,
        reply: Reply2,
    ) {
        let request = SecurityEventNotificationRequest {
            kind: event_type,
            timestamp: self.clock.now_utc(),
            tech_info,
            custom_data: None,
        };
        self.call::<SecurityEventNotification>(Origin2::Station, &request, move |_, result| {
            let answer = result
                .map(|response| Outcome2::Response {
                    payload: serde_json::to_value(&response).unwrap_or(Value::Null),
                })
                .map_err(call_failure);
            let _ = reply.send(answer);
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::{mpsc, oneshot};

    use super::*;
    use crate::ws::Connection;

    fn actor() -> Station2Actor {
        let (_, commands) = mpsc::channel(1);
        let (events, _) = tokio::sync::broadcast::channel(16);
        Station2Actor::new(
            crate::config::StationConfig::new("ws://localhost/ocpp").without_persistence(),
            Arc::new(crate::clock::SystemClock),
            commands,
            events,
            None,
        )
        .0
    }

    fn close(actor: &mut Station2Actor) {
        actor.on_connection_event(crate::ws::ConnectionEvent::Closed {
            code: Some(1001),
            reason: String::new(),
            by_us: false,
        });
    }

    fn queued_call(actor: &mut Station2Actor) -> oneshot::Receiver<Result<Value, CallError>> {
        let (reply, result) = oneshot::channel();
        actor.call_raw(
            Origin2::Raw,
            "Heartbeat",
            serde_json::json!({}),
            false,
            Box::new(move |_, outcome| {
                let _ = reply.send(outcome);
            }),
        );
        result
    }

    #[tokio::test]
    async fn close_preserves_received_callresult_before_waiter_runs() {
        let mut actor = actor();
        let (connection, mut reply) = Connection::test_reply_channel();
        actor.link.conn = Some(connection);
        let mut outcome = queued_call(&mut actor);
        let payload = serde_json::json!({"currentTime": "2026-11-10T00:00:00Z"});
        reply().send(Ok(payload.clone())).unwrap();
        close(&mut actor);
        assert_eq!(outcome.try_recv().unwrap().unwrap(), payload);
        assert!(actor.calls.pending.is_empty());
    }

    #[tokio::test]
    async fn close_preserves_received_callerror_before_waiter_runs() {
        let mut actor = actor();
        let (connection, mut reply) = Connection::test_reply_channel();
        actor.link.conn = Some(connection);
        let mut outcome = queued_call(&mut actor);
        reply()
            .send(Err(CallError::Rpc {
                action: "Heartbeat".to_owned(),
                code: crate::ocpp::frame::ErrorCode::InternalError,
                description: "test failure".to_owned(),
                details: serde_json::json!({}),
            }))
            .unwrap();
        close(&mut actor);
        assert!(matches!(
            outcome.try_recv().unwrap(),
            Err(CallError::Rpc { .. })
        ));
        assert!(actor.calls.pending.is_empty());
    }

    #[tokio::test]
    async fn close_fails_unanswered_call_once() {
        let mut actor = actor();
        let (connection, mut reply) = Connection::test_reply_channel();
        actor.link.conn = Some(connection);
        let mut outcome = queued_call(&mut actor);
        let pending = reply();
        close(&mut actor);
        assert!(matches!(
            outcome.try_recv().unwrap(),
            Err(CallError::Closed { code: Some(1001) })
        ));
        assert!(actor.calls.pending.is_empty());
        drop(pending);
    }

    #[tokio::test]
    async fn close_drains_more_replies_than_the_cooperative_budget() {
        let mut actor = actor();
        let (connection, mut reply) = Connection::test_reply_channel();
        actor.link.conn = Some(connection);
        let mut outcomes = Vec::new();
        for number in 0..300 {
            outcomes.push(queued_call(&mut actor));
            reply().send(Ok(serde_json::json!(number))).unwrap();
        }
        close(&mut actor);
        for (number, mut outcome) in outcomes.into_iter().enumerate() {
            assert_eq!(
                outcome.try_recv().unwrap().unwrap(),
                serde_json::json!(number)
            );
        }
        assert!(actor.calls.pending.is_empty());
        assert!(actor.calls.results.is_empty());
    }

    #[tokio::test]
    async fn a_timed_out_call_is_not_retried() {
        let mut actor = actor();
        let (connection, mut reply) = Connection::test_reply_channel();
        actor.link.conn = Some(connection);
        let mut outcome = queued_call(&mut actor);
        reply()
            .send(Err(CallError::Timeout {
                action: "Heartbeat".to_owned(),
                after: Duration::from_secs(1),
            }))
            .unwrap();
        close(&mut actor);
        assert!(matches!(
            outcome.try_recv().unwrap(),
            Err(CallError::Timeout { .. })
        ));
        // Nothing was queued again: the connection saw one CALL only.
        assert!(actor.calls.pending.is_empty());
        assert!(actor.calls.results.is_empty());
    }

    #[tokio::test]
    async fn the_gate_follows_the_registration_state() {
        let mut actor = actor();
        let gate = |actor: &Station2Actor, origin| actor.may_send("Heartbeat", origin).is_ok();
        let boot =
            |actor: &Station2Actor| actor.may_send("BootNotification", Origin2::Station).is_ok();
        actor.state.registration = None;
        assert!(
            boot(&actor) && !gate(&actor, Origin2::Station) && !gate(&actor, Origin2::Triggered)
        );
        assert!(gate(&actor, Origin2::Raw));
        actor.state.registration = Some(RegistrationStatus::Pending);
        assert!(
            boot(&actor) && !gate(&actor, Origin2::Station) && gate(&actor, Origin2::Triggered)
        );
        actor.state.registration = Some(RegistrationStatus::Rejected);
        assert!(
            boot(&actor) && !gate(&actor, Origin2::Station) && !gate(&actor, Origin2::Triggered)
        );
        assert!(gate(&actor, Origin2::Raw));
        actor.state.registration = Some(RegistrationStatus::Accepted);
        assert!(gate(&actor, Origin2::Station) && gate(&actor, Origin2::Triggered));
    }

    #[tokio::test]
    async fn a_gated_call_fails_at_once_without_reaching_the_connection() {
        let mut actor = actor();
        let (connection, _reply) = Connection::test_reply_channel();
        actor.link.conn = Some(connection);
        let (sender, mut outcome) = oneshot::channel();
        actor.call_raw(
            Origin2::Station,
            "Heartbeat",
            serde_json::json!({}),
            false,
            Box::new(move |_, result| {
                let _ = sender.send(result);
            }),
        );
        assert!(matches!(
            outcome.try_recv().unwrap(),
            Err(CallError::NotRegistered { .. })
        ));
        assert!(actor.calls.pending.is_empty());
    }
}
