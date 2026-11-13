//! State shared between the public handle and the connection tasks.
//!
//! The mutex is held only for short, synchronous sections and never across an
//! `.await`. Waiters register interest in [`Shared::changed`] *before* checking the
//! condition, so a change made between the check and the wait is not lost.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::Utc;
use kirby_csms_core::{Fault, Reply, Responder, SubprotocolPolicy, TransactionIds, default_reply};
use kirby_ocpp_core::ocpp::Frame;
use serde_json::Value;
use tokio::sync::{Notify, mpsc, oneshot};

use crate::error::{TestkitError, TestkitResult};
use crate::options::CsmsOptions;
use crate::record::{ConnectionInfo, Received, charge_point_id_from_path};

/// Close code used for injected and dropped connections.
pub(crate) const CLOSE_CODE_ERROR: u16 = 1011;

/// What a connection task must write to its socket.
#[derive(Debug)]
pub(crate) enum Outbound {
    /// A complete OCPP-J frame as text.
    Text(String),
    /// A close frame; the connection task stops after sending it.
    Close { code: u16, reason: String },
}

/// A CSMS-originated CALL waiting for its CALLRESULT or CALLERROR.
pub(crate) struct Pending {
    pub(crate) connection: u64,
    pub(crate) reply: oneshot::Sender<Frame>,
}

pub(crate) struct State {
    pub(crate) received: Vec<Received>,
    pub(crate) connections: Vec<ConnectionInfo>,
    pub(crate) next_connection_id: u64,
    /// Open connections. The highest id is the "current" connection that
    /// CSMS-originated calls are sent to.
    pub(crate) open: BTreeMap<u64, mpsc::UnboundedSender<Outbound>>,
    pub(crate) pending: HashMap<String, Pending>,
    pub(crate) faults: Vec<Fault>,
    pub(crate) responders: HashMap<String, Responder>,
    next_message_id: u64,
    transaction_ids: TransactionIds,
    next_transaction_id: i32,
}

impl State {
    /// The most recent open connection and its outbound channel.
    pub(crate) fn current(&self) -> Option<(u64, mpsc::UnboundedSender<Outbound>)> {
        self.open
            .iter()
            .next_back()
            .map(|(id, tx)| (*id, tx.clone()))
    }

    /// Consumes one occurrence of the first matching fault, if any.
    fn take_fault(&mut self, action: &str) -> Option<Reply> {
        let index = self
            .faults
            .iter()
            .position(|fault| fault.action == action && fault.remaining > 0)?;
        let fault = &mut self.faults[index];
        fault.remaining -= 1;
        let reply = fault.reply.clone();
        self.faults.retain(|fault| fault.remaining > 0);
        Some(reply)
    }

    pub(crate) fn next_message_id(&mut self) -> String {
        self.next_message_id += 1;
        format!("csms-{}", self.next_message_id)
    }

    fn allocate_transaction_id(&mut self) -> i32 {
        match self.transaction_ids {
            TransactionIds::Fixed(id) => id,
            TransactionIds::Incrementing(_) => {
                let id = self.next_transaction_id;
                self.next_transaction_id += 1;
                id
            }
        }
    }
}

pub(crate) struct Shared {
    state: Mutex<State>,
    pub(crate) changed: Notify,
    pub(crate) subprotocol: SubprotocolPolicy,
    heartbeat_interval: u32,
}

impl Shared {
    pub(crate) fn new(options: &CsmsOptions) -> Self {
        let next_transaction_id = match options.transaction_ids {
            TransactionIds::Fixed(id) | TransactionIds::Incrementing(id) => id,
        };
        Self {
            state: Mutex::new(State {
                received: Vec::new(),
                connections: Vec::new(),
                next_connection_id: 0,
                open: BTreeMap::new(),
                pending: HashMap::new(),
                faults: Vec::new(),
                responders: HashMap::new(),
                next_message_id: 0,
                transaction_ids: options.transaction_ids,
                next_transaction_id,
            }),
            changed: Notify::new(),
            subprotocol: options.subprotocol.clone(),
            heartbeat_interval: options.heartbeat_interval,
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn notify(&self) {
        self.changed.notify_waiters();
    }

    /// Registers an accepted connection and returns its id.
    pub(crate) fn register(
        &self,
        path: String,
        subprotocol: Option<String>,
        outbound: mpsc::UnboundedSender<Outbound>,
    ) -> u64 {
        let id = {
            let mut state = self.lock();
            state.next_connection_id += 1;
            let id = state.next_connection_id;
            state.connections.push(ConnectionInfo {
                id,
                charge_point_id: charge_point_id_from_path(&path),
                path,
                subprotocol,
                open: true,
            });
            state.open.insert(id, outbound);
            id
        };
        self.notify();
        id
    }

    /// Marks a connection closed and fails the CSMS calls still waiting on it.
    pub(crate) fn close_connection(&self, id: u64) {
        {
            let mut state = self.lock();
            state.open.remove(&id);
            if let Some(info) = state.connections.iter_mut().find(|info| info.id == id) {
                info.open = false;
            }
            // Dropping the senders makes the waiting `send_call` fail immediately.
            state.pending.retain(|_, pending| pending.connection != id);
        }
        self.notify();
    }

    /// Closes every connection (used on shutdown and drop).
    pub(crate) fn close_all(&self) {
        {
            let mut state = self.lock();
            state.open.clear();
            for info in &mut state.connections {
                info.open = false;
            }
            state.pending.clear();
        }
        self.notify();
    }

    /// Records a CP-originated CALL. Called before the reply is decided.
    fn record_call(&self, connection: u64, message_id: &str, action: &str, payload: Value) {
        {
            let mut state = self.lock();
            state.received.push(Received {
                ts: Utc::now(),
                connection,
                message_id: message_id.to_owned(),
                action: action.to_owned(),
                payload,
            });
        }
        self.notify();
    }

    /// Decides the reply to a CP-originated CALL: fault, then responder, then default.
    ///
    /// A responder runs outside the lock, so it may safely inspect this handle.
    fn decide(&self, action: &str, payload: &Value) -> Reply {
        let mut state = self.lock();
        if let Some(reply) = state.take_fault(action) {
            return reply;
        }
        let responder = state.responders.get(action).cloned();
        if let Some(responder) = responder {
            // Run outside the lock: the closure may call back into this handle.
            drop(state);
            return (*responder)(payload);
        }
        let transaction_id =
            (action == "StartTransaction").then(|| state.allocate_transaction_id());
        default_reply(action, payload, self.heartbeat_interval, transaction_id)
    }

    /// Handles one inbound text frame from `connection`. Returns the frame to send back, if any.
    pub(crate) fn handle_inbound(&self, connection: u64, text: &str) -> Option<Outbound> {
        match Frame::parse(text) {
            Ok(Frame::Call {
                id,
                action,
                payload,
            }) => {
                self.record_call(connection, &id, &action, payload.clone());
                reply_outbound(&id, self.decide(&action, &payload))
            }
            Ok(frame @ (Frame::CallResult { .. } | Frame::CallError { .. })) => {
                self.resolve_pending(connection, frame);
                None
            }
            Ok(_) => None,
            // A malformed CALL with a readable id gets a FormationViolation; anything else is ignored.
            Err(error) => error
                .reply()
                .map(|frame| Outbound::Text(frame.to_json_string())),
        }
    }

    fn resolve_pending(&self, connection: u64, frame: Frame) {
        let pending = {
            let mut state = self.lock();
            let owned = state
                .pending
                .get(frame.id())
                .is_some_and(|pending| pending.connection == connection);
            if owned {
                state.pending.remove(frame.id())
            } else {
                None
            }
        };
        if let Some(pending) = pending {
            // The waiter may have timed out already; that is fine.
            let _ = pending.reply.send(frame);
        }
    }
}

fn reply_outbound(id: &str, reply: Reply) -> Option<Outbound> {
    match reply {
        Reply::Result(payload) => Some(Outbound::Text(
            Frame::call_result(id, payload).to_json_string(),
        )),
        Reply::Error(code, description) => Some(Outbound::Text(
            Frame::call_error(id, code, description).to_json_string(),
        )),
        Reply::Silence => None,
        Reply::Close => Some(Outbound::Close {
            code: CLOSE_CODE_ERROR,
            reason: "injected close".to_owned(),
        }),
    }
}

/// Waits until `check` returns `Some`, or fails after `timeout`.
pub(crate) async fn wait_until<T>(
    shared: &Shared,
    what: &str,
    timeout: Duration,
    mut check: impl FnMut(&State) -> Option<T>,
) -> TestkitResult<T> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        // Created before the check, so a notification sent after the check is not missed.
        let changed = shared.changed.notified();
        if let Some(value) = check(&shared.lock()) {
            return Ok(value);
        }
        if tokio::time::timeout_at(deadline, changed).await.is_err() {
            return Err(TestkitError::Timeout {
                what: what.to_owned(),
                timeout,
            });
        }
    }
}

/// The outbound channel and reply receiver of a new CSMS-originated call.
pub(crate) struct StartedCall {
    pub(crate) id: String,
    pub(crate) outbound: mpsc::UnboundedSender<Outbound>,
    pub(crate) reply: oneshot::Receiver<Frame>,
}

impl Shared {
    /// Registers a pending CSMS call on the current connection and returns what is needed to send it.
    pub(crate) fn start_call(&self) -> TestkitResult<StartedCall> {
        let mut state = self.lock();
        let (connection, outbound) = state.current().ok_or(TestkitError::NotConnected)?;
        let id = state.next_message_id();
        let (reply, receiver) = oneshot::channel();
        state
            .pending
            .insert(id.clone(), Pending { connection, reply });
        Ok(StartedCall {
            id,
            outbound,
            reply: receiver,
        })
    }

    pub(crate) fn cancel_call(&self, id: &str) {
        self.lock().pending.remove(id);
    }

    pub(crate) fn set_responder(&self, action: &str, responder: Responder) {
        self.lock().responders.insert(action.to_owned(), responder);
    }

    pub(crate) fn inject(&self, fault: Fault) {
        if fault.remaining > 0 {
            self.lock().faults.push(fault);
        }
    }
}
