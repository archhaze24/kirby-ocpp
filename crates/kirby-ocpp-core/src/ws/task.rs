//! The connection task: owns the socket, the outbound CALL queue, the pending table
//! (our CALLs awaiting an answer) and the incoming table (the CSMS's CALLs awaiting
//! our reply).
//!
//! Everything runs in one `select!` loop. Each step is handled by a method that
//! returns `Err(Stop)` when the connection must end; [`Task::finish`] then fails all
//! outstanding calls and emits the final `Closed` event.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::Utc;
use futures_util::sink::Sink;
use futures_util::stream::Stream;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, Interval, interval_at, sleep_until, timeout};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Bytes, Error as WsError, Message};
use tracing::debug;
use uuid::Uuid;

use super::{ConnectOptions, ConnectionEvent, EVENT_CAPACITY, IncomingCall, schemas};
use crate::error::CallError;
use crate::ocpp::frame::{Direction, ErrorCode, Frame, FrameRecord, redact_secrets};
use crate::ocpp::responder::ReplyHandle;

/// Close code reported when the connection ended without a close handshake.
const ABNORMAL_CLOSE: u16 = 1006;
/// Close code of a normal, client-initiated close.
const NORMAL_CLOSE: u16 = 1000;
/// How long a close handshake may take before the socket is dropped.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Requests from [`super::Connection`] handles to the task.
pub(super) enum Command {
    Call(OutboundCall),
    Close,
}

/// A CALL waiting for its turn on the wire.
pub(super) struct OutboundCall {
    pub action: String,
    pub payload: Value,
    pub timeout: Duration,
    pub reply: oneshot::Sender<Result<Value, CallError>>,
}

/// A CALL we sent and have not got an answer to.
struct Pending {
    action: String,
    timeout: Duration,
    deadline: Instant,
    reply: oneshot::Sender<Result<Value, CallError>>,
}

/// Why the loop ends.
enum Stop {
    /// `close()` was called, every handle was dropped, or the owner stopped
    /// listening to events. Performs the close handshake.
    Requested,
    /// The peer sent a Close frame; tungstenite has queued our reply.
    Peer(Option<CloseFrame>),
    /// The socket broke, or the task terminated it. No close handshake.
    Abort { reason: String, by_us: bool },
}

type Flow = Result<(), Stop>;

/// One input to the loop, produced by `select!`.
enum Step {
    Command(Option<Command>),
    Reply(Option<Frame>),
    Socket(Option<Result<Message, WsError>>),
    Timeouts,
    Ping,
}

/// Starts the task for an established WebSocket. Returns the open flag, the command
/// sender and the event receiver.
pub(super) fn spawn<S>(
    ws: WebSocketStream<S>,
    options: &ConnectOptions,
) -> (
    Arc<AtomicBool>,
    mpsc::UnboundedSender<Command>,
    mpsc::Receiver<ConnectionEvent>,
)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let open = Arc::new(AtomicBool::new(true));
    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    let (events_tx, events_rx) = mpsc::channel(EVENT_CAPACITY);
    let (replies_tx, replies_rx) = mpsc::unbounded_channel();
    let task = Task {
        serialize: options.serialize_calls,
        show_secrets: options.show_secrets,
        ping_interval: options.ping_interval,
        open: Arc::clone(&open),
        events: events_tx,
        replies: replies_tx,
        queue: VecDeque::new(),
        pending: HashMap::new(),
        incoming: HashMap::new(),
        awaiting_pong: false,
    };
    tokio::spawn(task.run(ws, commands_rx, replies_rx));
    (open, commands_tx, events_rx)
}

struct Task {
    serialize: bool,
    show_secrets: bool,
    ping_interval: Option<Duration>,
    open: Arc<AtomicBool>,
    events: mpsc::Sender<ConnectionEvent>,
    /// Cloned into every [`ReplyHandle`]; replies come back through `replies`.
    replies: mpsc::UnboundedSender<Frame>,
    queue: VecDeque<OutboundCall>,
    /// Message id -> our CALL awaiting CALLRESULT/CALLERROR.
    pending: HashMap<String, Pending>,
    /// Message id -> action of a CSMS CALL we have not replied to yet.
    incoming: HashMap<String, String>,
    awaiting_pong: bool,
}

impl Task {
    async fn run<S>(
        mut self,
        ws: WebSocketStream<S>,
        mut commands: mpsc::UnboundedReceiver<Command>,
        mut replies: mpsc::UnboundedReceiver<Frame>,
    ) where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let (mut sink, mut stream) = ws.split();
        // A zero period would panic in `interval_at`; it disables pings, as in `StationConfig`.
        let mut ping = self
            .ping_interval
            .filter(|period| !period.is_zero())
            .map(|period| interval_at(Instant::now() + period, period));

        let stop = loop {
            let deadline = self.next_deadline();
            // Biased: our replies to CSMS CALLs go out before any CALL we queued after
            // them (a TriggerMessage.conf must precede the message it triggers).
            let step = tokio::select! {
                biased;
                reply = replies.recv() => Step::Reply(reply),
                command = commands.recv() => Step::Command(command),
                message = stream.next() => Step::Socket(message),
                () = until(deadline) => Step::Timeouts,
                () = tick(&mut ping) => Step::Ping,
            };
            if let Err(stop) = self.handle(step, &mut sink).await {
                break stop;
            }
        };

        self.finish(stop, &mut sink, &mut stream, &mut commands)
            .await;
    }

    async fn handle<W>(&mut self, step: Step, sink: &mut W) -> Flow
    where
        W: Sink<Message, Error = WsError> + Unpin,
    {
        match step {
            Step::Command(Some(Command::Call(call))) => {
                // Withdraw queued CALLs whose caller gave up, so a silent peer cannot make
                // abandoned CALLs pile up.
                self.queue.retain(|queued| !queued.reply.is_closed());
                self.queue.push_back(call);
                self.pump(sink).await
            }
            Step::Command(Some(Command::Close) | None) => Err(Stop::Requested),
            Step::Reply(Some(frame)) => self.reply(frame, sink).await,
            Step::Reply(None) => Ok(()),
            Step::Socket(Some(Ok(message))) => self.incoming_message(message, sink).await,
            Step::Socket(Some(Err(error))) => Err(abort(&error)),
            Step::Socket(None) => Err(Stop::Abort {
                reason: "connection lost".to_owned(),
                by_us: false,
            }),
            Step::Timeouts => self.expire(sink).await,
            Step::Ping => self.ping(sink).await,
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.pending.values().map(|pending| pending.deadline).min()
    }

    /// Sends queued CALLs while the serialization rule allows it.
    async fn pump<W>(&mut self, sink: &mut W) -> Flow
    where
        W: Sink<Message, Error = WsError> + Unpin,
    {
        while !self.queue.is_empty() && (!self.serialize || self.pending.is_empty()) {
            let Some(call) = self.queue.pop_front() else {
                break;
            };
            if call.reply.is_closed() {
                continue;
            }
            let OutboundCall {
                action,
                payload,
                timeout,
                reply,
            } = call;
            let id = Uuid::new_v4().to_string();
            let frame = Frame::call(id.clone(), action.clone(), payload);
            if let Err(stop) = self.write(frame, Some(action.clone()), sink).await {
                let _ = reply.send(Err(CallError::Closed {
                    code: Some(ABNORMAL_CLOSE),
                }));
                return Err(stop);
            }
            // The clock starts when the CALL is on the wire.
            self.pending.insert(
                id,
                Pending {
                    action,
                    timeout,
                    deadline: Instant::now() + timeout,
                    reply,
                },
            );
        }
        Ok(())
    }

    /// Writes one frame and reports it as [`ConnectionEvent::Frame`].
    async fn write<W>(&self, frame: Frame, action: Option<String>, sink: &mut W) -> Flow
    where
        W: Sink<Message, Error = WsError> + Unpin,
    {
        sink.send(Message::Text(frame.to_json_string().into()))
            .await
            .map_err(|error| abort(&error))?;
        let record = FrameRecord {
            ts: Utc::now(),
            direction: Direction::Out,
            msg_id: Some(frame.id().to_owned()),
            action,
            frame: self.recorded(&frame),
        };
        self.emit(ConnectionEvent::Frame(record)).await
    }

    /// The frame as it is recorded: secrets hidden unless `show_secrets`.
    fn recorded(&self, frame: &Frame) -> Value {
        if self.show_secrets {
            frame.to_value()
        } else {
            redact_secrets(frame).to_value()
        }
    }

    /// Writes our reply to a CSMS CALL, validated against the response schema.
    async fn reply<W>(&mut self, frame: Frame, sink: &mut W) -> Flow
    where
        W: Sink<Message, Error = WsError> + Unpin,
    {
        let id = frame.id().to_owned();
        let Some(action) = self.incoming.remove(&id) else {
            debug!(message_id = %id, "dropping a reply to an unknown or answered CALL");
            return Ok(());
        };
        let frame = match frame {
            Frame::CallResult { id, payload } => {
                match schemas().validate_response(&action, &payload) {
                    Ok(()) => Frame::CallResult { id, payload },
                    Err(violation) => {
                        self.warn(format!(
                            "reply to {action} violates the OCPP 1.6J schema, InternalError sent instead: {violation}"
                        ))
                        .await?;
                        Frame::call_error(id, ErrorCode::InternalError, violation.errors.join("; "))
                    }
                }
            }
            other => other,
        };
        self.write(frame, Some(action), sink).await
    }

    async fn incoming_message<W>(&mut self, message: Message, sink: &mut W) -> Flow
    where
        W: Sink<Message, Error = WsError> + Unpin,
    {
        match message {
            Message::Text(text) => self.incoming_text(text.as_str(), sink).await,
            Message::Binary(bytes) => match std::str::from_utf8(&bytes) {
                Ok(text) => self.incoming_text(text, sink).await,
                Err(_) => {
                    self.warn("binary WebSocket message ignored, OCPP-J is text only".to_owned())
                        .await
                }
            },
            // tungstenite queued the pong; the flush writes it out.
            Message::Ping(_) => sink.flush().await.map_err(|error| abort(&error)),
            Message::Pong(_) => {
                self.awaiting_pong = false;
                Ok(())
            }
            Message::Close(frame) => Err(Stop::Peer(frame)),
            Message::Frame(_) => Ok(()),
        }
    }

    async fn incoming_text<W>(&mut self, text: &str, sink: &mut W) -> Flow
    where
        W: Sink<Message, Error = WsError> + Unpin,
    {
        let frame = match Frame::parse(text) {
            Ok(frame) => frame,
            Err(error) => {
                let record = FrameRecord {
                    ts: Utc::now(),
                    direction: Direction::In,
                    msg_id: error.message_id.clone(),
                    action: None,
                    frame: Value::String(text.to_owned()),
                };
                self.emit(ConnectionEvent::Frame(record)).await?;
                self.warn(format!("invalid OCPP-J frame: {error}")).await?;
                return match error.reply() {
                    Some(reply) => self.write(reply, None, sink).await,
                    None => Ok(()),
                };
            }
        };

        let action = match &frame {
            Frame::Call { action, .. } => Some(action.clone()),
            other => self
                .pending
                .get(other.id())
                .map(|pending| pending.action.clone()),
        };
        let record = FrameRecord {
            ts: Utc::now(),
            direction: Direction::In,
            msg_id: Some(frame.id().to_owned()),
            action,
            frame: self.recorded(&frame),
        };
        self.emit(ConnectionEvent::Frame(record)).await?;

        match frame {
            Frame::Call {
                id,
                action,
                payload,
            } => self.incoming_call(id, action, payload, sink).await,
            Frame::CallResult { id, payload } => self.call_result(&id, payload, sink).await,
            Frame::CallError {
                id,
                code,
                description,
                details,
            } => self.call_error(&id, code, description, details, sink).await,
        }
    }

    /// Validates a CSMS CALL. Invalid ones are answered here; valid ones are delivered.
    async fn incoming_call<W>(
        &mut self,
        id: String,
        action: String,
        payload: Value,
        sink: &mut W,
    ) -> Flow
    where
        W: Sink<Message, Error = WsError> + Unpin,
    {
        // A CALL id that is still being handled is a retransmission: ignore it, the one reply
        // for the first copy goes out when the owner answers.
        if self.incoming.contains_key(&id) {
            return self
                .warn(format!(
                    "ignored duplicate CALL {id} ({action}): the first one is still being handled"
                ))
                .await;
        }
        let rejection = if schemas().knows_action(&action) {
            schemas()
                .validate_request(&action, &payload)
                .err()
                .map(|violation| (violation.error_code(), violation.errors.join("; ")))
        } else {
            Some((
                ErrorCode::NotImplemented,
                format!("{action} is not an OCPP 1.6J action"),
            ))
        };
        if let Some((code, description)) = rejection {
            self.warn(format!(
                "rejected CALL {id} ({action}) with {code}: {description}"
            ))
            .await?;
            let frame = Frame::call_error(id, code, description);
            return self.write(frame, Some(action), sink).await;
        }

        let reply = ReplyHandle::new(id.clone(), self.replies.clone());
        self.incoming.insert(id.clone(), action.clone());
        self.emit(ConnectionEvent::Call(IncomingCall {
            id,
            action,
            payload,
            reply,
        }))
        .await
    }

    async fn call_result<W>(&mut self, id: &str, payload: Value, sink: &mut W) -> Flow
    where
        W: Sink<Message, Error = WsError> + Unpin,
    {
        let Some(pending) = self.pending.remove(id) else {
            debug!(message_id = %id, "CALLRESULT for an unknown or expired CALL ignored");
            return Ok(());
        };
        let result = match schemas().validate_response(&pending.action, &payload) {
            Ok(()) => Ok(payload),
            Err(violation) => {
                self.warn(format!(
                    "CALLRESULT for {} violates the OCPP 1.6J schema: {violation}",
                    pending.action
                ))
                .await?;
                Err(CallError::Schema(violation))
            }
        };
        let _ = pending.reply.send(result);
        self.pump(sink).await
    }

    async fn call_error<W>(
        &mut self,
        id: &str,
        code: ErrorCode,
        description: String,
        details: Value,
        sink: &mut W,
    ) -> Flow
    where
        W: Sink<Message, Error = WsError> + Unpin,
    {
        let Some(pending) = self.pending.remove(id) else {
            debug!(message_id = %id, "CALLERROR for an unknown or expired CALL ignored");
            return Ok(());
        };
        let _ = pending.reply.send(Err(CallError::Rpc {
            action: pending.action,
            code,
            description,
            details,
        }));
        self.pump(sink).await
    }

    /// Fails the CALLs whose response did not arrive in time. A late response for
    /// them is then unknown and ignored.
    async fn expire<W>(&mut self, sink: &mut W) -> Flow
    where
        W: Sink<Message, Error = WsError> + Unpin,
    {
        let now = Instant::now();
        let due: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            if let Some(pending) = self.pending.remove(&id) {
                let _ = pending.reply.send(Err(CallError::Timeout {
                    action: pending.action,
                    after: pending.timeout,
                }));
            }
        }
        self.pump(sink).await
    }

    /// Watchdog tick: a ping whose pong never came terminates the socket.
    async fn ping<W>(&mut self, sink: &mut W) -> Flow
    where
        W: Sink<Message, Error = WsError> + Unpin,
    {
        if self.awaiting_pong {
            return Err(Stop::Abort {
                reason: "WebSocket pong timed out".to_owned(),
                by_us: true,
            });
        }
        self.awaiting_pong = true;
        sink.send(Message::Ping(Bytes::new()))
            .await
            .map_err(|error| abort(&error))
    }

    async fn emit(&self, event: ConnectionEvent) -> Flow {
        self.events.send(event).await.map_err(|_| Stop::Requested)
    }

    async fn warn(&self, message: String) -> Flow {
        self.emit(ConnectionEvent::ProtocolWarning(message)).await
    }

    /// Ends the connection. Outstanding calls fail first (with the close code known
    /// now), so callers do not wait for a close handshake with a silent peer; then
    /// the socket is closed and the final `Closed` event is emitted.
    async fn finish<W, R>(
        &mut self,
        stop: Stop,
        sink: &mut W,
        stream: &mut R,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) where
        W: Sink<Message, Error = WsError> + Unpin,
        R: Stream<Item = Result<Message, WsError>> + Unpin,
    {
        self.open.store(false, Ordering::SeqCst);
        let known_code = match &stop {
            Stop::Requested => Some(NORMAL_CLOSE),
            Stop::Peer(frame) => frame.as_ref().map(|frame| u16::from(frame.code)),
            Stop::Abort { .. } => Some(ABNORMAL_CLOSE),
        };
        self.fail_all(known_code);
        // Calls sent to the channel but not yet taken fail too; new ones get NotConnected.
        commands.close();
        while let Ok(command) = commands.try_recv() {
            if let Command::Call(call) = command {
                let _ = call.reply.send(Err(CallError::Closed { code: known_code }));
            }
        }
        self.incoming.clear();

        let (code, reason, by_us) = match stop {
            Stop::Requested => {
                let (code, reason) = close_handshake(sink, stream).await;
                (code, reason, true)
            }
            Stop::Peer(frame) => {
                let _ = timeout(CLOSE_TIMEOUT, sink.flush()).await;
                let (code, reason) = frame_code_and_reason(frame);
                (code, reason, false)
            }
            Stop::Abort { reason, by_us } => (Some(ABNORMAL_CLOSE), reason, by_us),
        };
        let _ = self
            .events
            .send(ConnectionEvent::Closed {
                code,
                reason,
                by_us,
            })
            .await;
    }

    fn fail_all(&mut self, code: Option<u16>) {
        for (_, pending) in self.pending.drain() {
            let _ = pending.reply.send(Err(CallError::Closed { code }));
        }
        for call in self.queue.drain(..) {
            let _ = call.reply.send(Err(CallError::Closed { code }));
        }
    }
}

fn abort(error: &WsError) -> Stop {
    Stop::Abort {
        reason: error.to_string(),
        by_us: false,
    }
}

fn frame_code_and_reason(frame: Option<CloseFrame>) -> (Option<u16>, String) {
    match frame {
        Some(frame) => (Some(u16::from(frame.code)), frame.reason.to_string()),
        None => (None, String::new()),
    }
}

/// Sends a Close frame (normal closure) and waits, bounded, for the peer's reply.
async fn close_handshake<W, R>(sink: &mut W, stream: &mut R) -> (Option<u16>, String)
where
    W: Sink<Message, Error = WsError> + Unpin,
    R: Stream<Item = Result<Message, WsError>> + Unpin,
{
    let close = CloseFrame {
        code: CloseCode::Normal,
        reason: "".into(),
    };
    if sink.send(Message::Close(Some(close))).await.is_err() {
        return (
            Some(ABNORMAL_CLOSE),
            "cannot send the close frame".to_owned(),
        );
    }
    match timeout(CLOSE_TIMEOUT, await_peer_close(stream)).await {
        Ok(Some(frame)) => frame_code_and_reason(frame),
        Ok(None) => (
            Some(ABNORMAL_CLOSE),
            "connection ended without a close reply".to_owned(),
        ),
        Err(_) => (Some(ABNORMAL_CLOSE), "close handshake timed out".to_owned()),
    }
}

async fn await_peer_close<R>(stream: &mut R) -> Option<Option<CloseFrame>>
where
    R: Stream<Item = Result<Message, WsError>> + Unpin,
{
    while let Some(message) = stream.next().await {
        match message {
            Ok(Message::Close(frame)) => return Some(frame),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => sleep_until(at).await,
        None => std::future::pending::<()>().await,
    }
}

async fn tick(ping: &mut Option<Interval>) {
    match ping {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}
